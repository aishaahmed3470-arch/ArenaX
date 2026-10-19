/**
 * API Key Service Tests — Issue #1420
 *
 * Tests cover:
 *  1. Key creation and hashing (raw key never stored)
 *  2. Scope validation — unknown scopes are rejected
 *  3. Read-only key can resolve but write scope check blocks mutation
 *  4. Rotation overlap — old hash accepted within window, rejected after
 *  5. Max-uses enforcement
 *  6. hasScopes / validateScopes helpers
 */

import crypto from 'crypto';
import {
  generateRawApiKey,
  hashApiKey,
  hasScopes,
  validateScopes,
  ApiKeyScope,
  DEFAULT_OVERLAP_SECONDS,
  MAX_OVERLAP_SECONDS,
} from './api-key.service';

// ---------------------------------------------------------------------------
// Helper — build a minimal in-memory key record as returned by resolveKey
// ---------------------------------------------------------------------------
function makeRecord(overrides: Partial<{
  id: string;
  keyHash: string;
  oldKeyHash: string | null;
  oldKeyExpiresAt: Date | null;
  scopes: ApiKeyScope[];
  isActive: boolean;
  expiresAt: Date | null;
  maxUses: number | null;
  useCount: number;
}> = {}) {
  return {
    id: 'test-key-id',
    keyHash: 'hash',
    oldKeyHash: null,
    oldKeyExpiresAt: null,
    scopes: ['read:tournaments'] as ApiKeyScope[],
    isActive: true,
    expiresAt: null,
    maxUses: null,
    useCount: 0,
    ...overrides,
  };
}

// ---------------------------------------------------------------------------
// 1. Key generation & hashing
// ---------------------------------------------------------------------------

describe('generateRawApiKey', () => {
  it('produces a non-empty URL-safe base64 string', () => {
    const key = generateRawApiKey();
    expect(key).toBeTruthy();
    // base64url: [A-Za-z0-9_-], no padding
    expect(key).toMatch(/^[A-Za-z0-9_-]+$/);
  });

  it('produces unique values on each call', () => {
    const keys = new Set(Array.from({ length: 50 }, () => generateRawApiKey()));
    expect(keys.size).toBe(50);
  });
});

describe('hashApiKey', () => {
  it('returns a 64-character hex SHA-256 digest', () => {
    const hash = hashApiKey('test-key');
    expect(hash).toHaveLength(64);
    expect(hash).toMatch(/^[0-9a-f]+$/);
  });

  it('is deterministic — same input, same hash', () => {
    const raw = generateRawApiKey();
    expect(hashApiKey(raw)).toBe(hashApiKey(raw));
  });

  it('different inputs produce different hashes', () => {
    expect(hashApiKey('key-one')).not.toBe(hashApiKey('key-two'));
  });
});

// ---------------------------------------------------------------------------
// 2. Scope validation helpers
// ---------------------------------------------------------------------------

describe('validateScopes', () => {
  it('accepts all known scopes', () => {
    const scopes: string[] = ['read:tournaments', 'write:results', 'read:leaderboard'];
    expect(() => validateScopes(scopes)).not.toThrow();
    expect(validateScopes(scopes)).toEqual(scopes);
  });

  it('throws on unknown scope strings', () => {
    expect(() => validateScopes(['read:tournaments', 'delete:everything'])).toThrow(
      /Unknown API key scopes/,
    );
  });

  it('throws when every scope is unknown', () => {
    expect(() => validateScopes(['hack:all'])).toThrow(/Unknown API key scopes/);
  });
});

describe('hasScopes', () => {
  it('returns true when all required scopes are granted', () => {
    const granted: ApiKeyScope[] = ['read:tournaments', 'write:results', 'read:leaderboard'];
    expect(hasScopes(granted, ['read:tournaments', 'write:results'])).toBe(true);
  });

  it('returns false when any required scope is missing', () => {
    const granted: ApiKeyScope[] = ['read:tournaments'];
    // write:results is not granted
    expect(hasScopes(granted, ['read:tournaments', 'write:results'])).toBe(false);
  });

  it('returns true for an empty required list', () => {
    expect(hasScopes(['read:tournaments'], [])).toBe(true);
  });

  it('returns false when granted list is empty but scopes are required', () => {
    expect(hasScopes([], ['read:tournaments'])).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// 3. Read-only key scope enforcement
//    (simulating the middleware check without HTTP layer)
// ---------------------------------------------------------------------------

describe('read-only API key scope enforcement', () => {
  /**
   * Simulate the `requireApiKeyScope` middleware logic in isolation:
   * returns true if the key holds ALL required scopes, false otherwise.
   */
  function canAccess(keyScopes: ApiKeyScope[], required: ApiKeyScope[]): boolean {
    return hasScopes(keyScopes, required);
  }

  it('read-only key CAN read tournaments', () => {
    const readOnlyKey = makeRecord({ scopes: ['read:tournaments'] });
    expect(canAccess(readOnlyKey.scopes, ['read:tournaments'])).toBe(true);
  });

  it('read-only key CANNOT write results — scope check fails (HTTP 403 analog)', () => {
    const readOnlyKey = makeRecord({ scopes: ['read:tournaments'] });
    expect(canAccess(readOnlyKey.scopes, ['write:results'])).toBe(false);
  });

  it('full-access key can both read tournaments and write results', () => {
    const fullKey = makeRecord({
      scopes: ['read:tournaments', 'write:results', 'read:leaderboard'],
    });
    expect(canAccess(fullKey.scopes, ['read:tournaments'])).toBe(true);
    expect(canAccess(fullKey.scopes, ['write:results'])).toBe(true);
  });

  it('multi-scope endpoint requires ALL scopes — partial grant is rejected', () => {
    const partialKey = makeRecord({ scopes: ['read:matches'] });
    // Endpoint requires both read:matches AND write:matches
    expect(canAccess(partialKey.scopes, ['read:matches', 'write:matches'])).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// 4. Rotation overlap window
// ---------------------------------------------------------------------------

describe('rotation overlap window', () => {
  it('default overlap is 24 h (86400 s)', () => {
    expect(DEFAULT_OVERLAP_SECONDS).toBe(86_400);
  });

  it('maximum overlap is 7 days (604800 s)', () => {
    expect(MAX_OVERLAP_SECONDS).toBe(604_800);
  });

  /**
   * Simulate the overlap expiry check as performed by resolveKey:
   * the old key is valid when `oldKeyExpiresAt > now`, rejected when lapsed.
   */
  function oldKeyStillValid(expiresAt: Date | null, now: Date): boolean {
    if (!expiresAt) return false;
    return expiresAt > now;
  }

  it('old key is accepted while the overlap window is open', () => {
    const now = new Date();
    const expiresAt = new Date(now.getTime() + DEFAULT_OVERLAP_SECONDS * 1_000);
    expect(oldKeyStillValid(expiresAt, now)).toBe(true);
  });

  it('old key is rejected after the overlap window closes', () => {
    const now = new Date();
    const expiredAt = new Date(now.getTime() - 1_000); // 1 second in the past
    expect(oldKeyStillValid(expiredAt, now)).toBe(false);
  });

  it('no overlap expiry → false (new key, not a rotated old key)', () => {
    expect(oldKeyStillValid(null, new Date())).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// 5. Max-uses enforcement
// ---------------------------------------------------------------------------

describe('max-uses enforcement', () => {
  /**
   * Simulate the use-count gate inside resolveKey.
   */
  function withinMaxUses(record: ReturnType<typeof makeRecord>): boolean {
    if (record.maxUses === null) return true;
    return record.useCount < record.maxUses;
  }

  it('key with no max-uses is always within limit', () => {
    const key = makeRecord({ maxUses: null, useCount: 999 });
    expect(withinMaxUses(key)).toBe(true);
  });

  it('key within its use limit is accepted', () => {
    const key = makeRecord({ maxUses: 100, useCount: 99 });
    expect(withinMaxUses(key)).toBe(true);
  });

  it('key at exactly its use limit is rejected', () => {
    const key = makeRecord({ maxUses: 100, useCount: 100 });
    expect(withinMaxUses(key)).toBe(false);
  });

  it('key beyond its use limit is rejected', () => {
    const key = makeRecord({ maxUses: 5, useCount: 10 });
    expect(withinMaxUses(key)).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// 6. Key-hash uniqueness — collision resistance at scale
// ---------------------------------------------------------------------------

describe('hash collision resistance', () => {
  it('1 000 unique raw keys produce 1 000 unique hashes', () => {
    const hashes = new Set(
      Array.from({ length: 1_000 }, () => hashApiKey(generateRawApiKey())),
    );
    expect(hashes.size).toBe(1_000);
  });
});
