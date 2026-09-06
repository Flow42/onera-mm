// One game's submenu is addressed by its identifier, which is only known at
// runtime, so this route is served through the static adapter's fallback
// rather than prerendered. Everything it shows still comes from the command
// bridge, exactly as the prerendered routes do.
export const prerender = false;
