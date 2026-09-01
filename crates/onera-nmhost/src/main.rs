//! Chromium Native Messaging host.
//!
//! Chromium starts this process, hands it a framed JSON message on stdin and
//! reads the reply from stdout. The host is a thin adapter: it validates the
//! message, calls one method on [`onera_app::Onera`] and encodes the result.
//! No filesystem, installation or conflict logic lives here.
//!
//! Two things are deliberately absent:
//!
//! * archives never travel over this transport — the extension only ever sends
//!   identifiers, and the native application does the downloading;
//! * the API key is never sent to the extension, in either direction.
//!
//! stdout belongs to the protocol, so logging goes to stderr, which Chromium
//! captures into the browser's own log.

#![forbid(unsafe_code)]

mod protocol;

use onera_app::{Onera, Paths};
use onera_core::ids::ProviderModId;
use onera_core::progress::CancelToken;
use protocol::{
    code_for, error_response, ok_response, read_message, validate, write_message, Command,
    ErrorCode, FramingError, Request, Response,
};
use std::io::{stdin, stdout};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    // stdout is the protocol channel; everything human-readable goes to stderr.
    let _ = onera_app::logging::init(None, onera_app::logging::LogFormat::Text, false);

    let paths = match Paths::discover() {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("onera-nmhost: cannot resolve XDG directories: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let onera = match Onera::new(paths).await {
        Ok(onera) => onera,
        Err(e) => {
            eprintln!("onera-nmhost: cannot start: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut input = stdin().lock();
    let mut output = stdout().lock();
    loop {
        match read_message(&mut input) {
            Ok(request) => {
                let response = handle(&onera, request).await;
                if let Err(e) = write_message(&mut output, &response) {
                    eprintln!("onera-nmhost: cannot write response: {e}");
                    return std::process::ExitCode::FAILURE;
                }
            }
            // A clean end of stream means the browser closed the port.
            Err(FramingError::Eof) => return std::process::ExitCode::SUCCESS,
            Err(e) => {
                // A malformed frame leaves the stream out of sync, so the only
                // safe response is to report it and stop.
                let response = error_response("unknown", ErrorCode::Malformed, e.to_string());
                let _ = write_message(&mut output, &response);
                eprintln!("onera-nmhost: {e}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
}

/// Dispatch one validated request.
async fn handle(onera: &Onera, request: Request) -> Response {
    if let Err(response) = validate(&request) {
        return response;
    }
    let id = request.id.clone();
    let cancel = CancelToken::new();
    let should_install = matches!(request.command, Command::DownloadAndInstall { .. });

    match request.command {
        Command::Ping => ok_response(
            &id,
            serde_json::json!({ "pong": true, "version": env!("CARGO_PKG_VERSION") }),
        ),

        Command::Status => match onera.is_authenticated().await {
            Ok(authenticated) => {
                let games = onera.local_games().await.unwrap_or_default();
                ok_response(
                    &id,
                    serde_json::json!({
                        "authenticated": authenticated,
                        "games": games
                            .iter()
                            .map(|g| serde_json::json!({
                                "id": g.id.to_string(),
                                "adapter": g.adapter_id,
                            }))
                            .collect::<Vec<_>>(),
                    }),
                )
            }
            Err(e) => error_response(&id, code_for(&e), e.to_string()),
        },

        Command::AddMod {
            game_domain,
            mod_id,
        } => {
            match onera
                .fetch_mod(&game_domain, &mod_id.as_str().into(), &cancel)
                .await
            {
                Ok(details) => match onera
                    .enqueue_add_mod(game_domain, mod_id.as_str().into())
                    .await
                {
                    Ok(request) => ok_response(
                        &id,
                        serde_json::json!({
                            "queued": true,
                            "request_id": request.id.to_string(),
                            "mod_id": details.mod_id.to_string(),
                            "name": details.name,
                            "author": details.author,
                            "files": details.files.len(),
                        }),
                    ),
                    Err(e) => error_response(&id, code_for(&e), e.to_string()),
                },
                Err(e) => error_response(&id, code_for(&e), e.to_string()),
            }
        }

        Command::Download {
            game_domain,
            mod_id,
            file_id,
        }
        | Command::DownloadAndInstall {
            game_domain,
            mod_id,
            file_id,
        } => {
            let details = match onera
                .fetch_mod(&game_domain, &mod_id.as_str().into(), &cancel)
                .await
            {
                Ok(details) => details,
                Err(e) => return error_response(&id, code_for(&e), e.to_string()),
            };

            // When the user did not name a file and more than one is plausible,
            // the host refuses to guess and asks the desktop app to prompt.
            let chosen = match &file_id {
                Some(wanted) => details
                    .files
                    .iter()
                    .find(|f| f.provider_file_id.as_str() == wanted),
                None if details.needs_file_selection() => None,
                None => details
                    .primary_file()
                    .or_else(|| details.selectable_files().next()),
            };

            let Some(file) = chosen else {
                if file_id.is_some() || details.selectable_files().count() == 0 {
                    return error_response(
                        &id,
                        ErrorCode::SelectionRequired,
                        format!(
                            "{} does not offer that downloadable file; choose one in Onera",
                            details.name
                        ),
                    );
                }
                return match onera
                    .enqueue_download_selection_request(
                        game_domain,
                        ProviderModId::new(mod_id),
                        should_install,
                    )
                    .await
                {
                    Ok(request) => ok_response(
                        &id,
                        serde_json::json!({
                            "queued": true,
                            "request_id": request.id.to_string(),
                            "mod_id": details.mod_id.to_string(),
                            "name": details.name,
                            "selection_required": true,
                            "install": should_install,
                        }),
                    ),
                    Err(e) => error_response(&id, code_for(&e), e.to_string()),
                };
            };

            // The durable inbox is the handoff: the popup may close and this
            // short-lived host process may exit without losing the request.
            match onera
                .enqueue_download_request(
                    game_domain,
                    ProviderModId::new(mod_id),
                    file.provider_file_id.clone(),
                    should_install,
                )
                .await
            {
                Ok(request) => ok_response(
                    &id,
                    serde_json::json!({
                        "queued": true,
                        "request_id": request.id.to_string(),
                        "mod_id": details.mod_id.to_string(),
                        "name": details.name,
                        "file_id": file.provider_file_id.as_str(),
                        "file_name": file.name,
                        "install": should_install,
                    }),
                ),
                Err(e) => error_response(&id, code_for(&e), e.to_string()),
            }
        }
    }
}
