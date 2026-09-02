/**
 * The bridge is the single narrowing point between untrusted backend output and
 * typed frontend data, so its error handling is tested directly.
 */
import { describe, expect, it, vi } from 'vitest';
import { BridgeError, commands, normaliseError, setBridge } from './bridge';

describe('normaliseError', () => {
  it('passes a BridgeError through unchanged', () => {
    const original = new BridgeError('not_found', 'gone');
    expect(normaliseError(original)).toBe(original);
  });

  it('accepts the structured error the Tauri commands return', () => {
    const error = normaliseError({ code: 'decision_required', message: '2 conflicts' });
    expect(error.code).toBe('decision_required');
    expect(error.message).toBe('2 conflicts');
  });

  it('accepts a bare string', () => {
    expect(normaliseError('something broke').code).toBe('internal');
    expect(normaliseError('something broke').message).toBe('something broke');
  });

  it('never produces an empty message', () => {
    for (const value of [null, undefined, 42, [], {}]) {
      const error = normaliseError(value);
      expect(error.message.length, String(value)).toBeGreaterThan(0);
      expect(error.code).toBe('internal');
    }
  });
});

describe('commands', () => {
  it('forwards arguments and returns typed results', async () => {
    const invoke = vi.fn().mockResolvedValue(true);
    setBridge({ invoke, listen: vi.fn() });

    await expect(commands.isAuthenticated()).resolves.toBe(true);
    expect(invoke).toHaveBeenCalledWith('is_authenticated', undefined);

    setBridge(null);
  });

  it('turns a rejection into a BridgeError', async () => {
    setBridge({
      invoke: vi.fn().mockRejectedValue({ code: 'not_authenticated', message: 'no key' }),
      listen: vi.fn(),
    });

    await expect(commands.account()).rejects.toBeInstanceOf(BridgeError);
    await expect(commands.account()).rejects.toMatchObject({ code: 'not_authenticated' });

    setBridge(null);
  });

  it('never sends the API key back to the frontend', async () => {
    const invoke = vi.fn().mockResolvedValue({ username: 'TestUser', premium: true });
    setBridge({ invoke, listen: vi.fn() });

    const account = await commands.setApiKey('secret-key-value');
    expect(JSON.stringify(account)).not.toContain('secret-key-value');

    setBridge(null);
  });

  it('uses the documented profile command names and camelCase arguments', async () => {
    const invoke = vi.fn().mockResolvedValue({});
    setBridge({ invoke, listen: vi.fn() });

    await commands.profiles('game-1');
    await commands.profileMembers('profile-1');
    await commands.createProfile('game-1', 'Quiet', undefined, 'profile-1');
    await commands.renameProfile('profile-1', 'Loud');
    await commands.deleteProfile('profile-1');
    await commands.addProfileMember('profile-1', 'mod-1');
    await commands.removeProfileMember('member-1');
    await commands.setMemberState('member-1', 'disabled');
    await commands.setMemberPin('member-1', true, 'known good');
    await commands.reorderProfileMember('member-1', -12);
    await commands.resolveDependencies('profile-1');
    await commands.planProfileActivation('profile-1');
    await commands.activateProfile('profile-1');

    expect(invoke.mock.calls).toEqual([
      ['profiles', { gameId: 'game-1' }],
      ['profile_members', { profileId: 'profile-1' }],
      [
        'create_profile',
        {
          gameId: 'game-1',
          name: 'Quiet',
          description: null,
          copyFromProfileId: 'profile-1',
        },
      ],
      ['rename_profile', { profileId: 'profile-1', name: 'Loud' }],
      ['delete_profile', { profileId: 'profile-1' }],
      ['add_profile_member', { profileId: 'profile-1', modId: 'mod-1', providerFileId: null }],
      ['remove_profile_member', { memberId: 'member-1' }],
      ['set_member_state', { memberId: 'member-1', desired: 'disabled' }],
      ['set_member_pin', { memberId: 'member-1', pinned: true, reason: 'known good' }],
      ['reorder_profile_member', { memberId: 'member-1', priority: -12 }],
      ['resolve_dependencies', { profileId: 'profile-1' }],
      ['plan_profile_activation', { profileId: 'profile-1' }],
      ['activate_profile', { profileId: 'profile-1' }],
    ]);

    setBridge(null);
  });

  it('preserves the stable conflict code for active-profile deletion', async () => {
    setBridge({
      invoke: vi.fn().mockRejectedValue({
        code: 'conflict',
        message: 'the active profile cannot be deleted',
      }),
      listen: vi.fn(),
    });

    await expect(commands.deleteProfile('active')).rejects.toMatchObject({ code: 'conflict' });
    setBridge(null);
  });
});
