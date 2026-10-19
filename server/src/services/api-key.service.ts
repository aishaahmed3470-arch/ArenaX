/**
 * API Key Service — Issue #1420
 *
 * Manages the full lifecycle of third-party API keys:
 *   - Scoped permission model (read:tournaments, write:results, etc.)
 *   - SHA-256 hashed storage — raw keys are never persisted
 *   - Zero-downtime rotation with a configurable overlap window
 *   - Per-request scope enforcement
 */

import crypto from 'crypto';
import { getDatabaseClient } from './database.service';
import { logger } from './logger.service';

// ---------------------------------------------------------------------------
// Scope catalogue
// ---------------------------------------------------------------------------

export const API_KEY_SCOPES = [
  'read:tournaments',
  'write:tournaments',
  'read:results',
  'write:results',
  'read:leaderboard',
  'read:players',
  'write:players',
  'read:matches',
  'write:matches',
  'admin:keys',
] as const;

export type ApiKeyScope = (typeof API_KEY_SCOPES)[number];

// ---------------------------------------------------------------------------
// Default & maximum overlap window
// ---------------------------------------------------------------------------

/** 24 hours (seconds) — both old and new key are valid during this window. */
export const DEFAULT_OVERLAP_SECONDS = 86_400;
/** 7 days (seconds) — hard ceiling for any overlap window. */
export const MAX_OVERLAP_SECONDS = 604_800;

function clampOverlap(requested?: number | null): number {
  const value = requested ?? DEFAULT_OVERLAP_SECONDS;
  return Math.min(Math.max(value, 0), MAX_OVERLAP_SECONDS);
}

// ---------------------------------------------------------------------------
// Hashing helpers
// ---------------------------------------------------------------------------

/** One-way SHA-256 hash of a raw API key string. */
export function hashApiKey(raw: string): string {
  return crypto.createHash('sha256').update(raw).digest('hex');
}

/** Generate a cryptographically secure URL-safe API key. */
export function generateRawApiKey(): string {
  return crypto.randomBytes(32).toString('base64url');
}

/** Constant-time comparison to prevent timing attacks. */
function safeEquals(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  return crypto.timingSafeEqual(Buffer.from(a), Buffer.from(b));
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

export interface CreateApiKeyDto {
  name: string;
  description?: string;
  scopes: ApiKeyScope[];
  expiresAt?: Date;
  overlapSeconds?: number;
  maxUses?: number;
}

export interface RotateApiKeyDto {
  reason?: string;
  overlapSeconds?: number;
}

export interface ApiKeyRecord {
  id: string;
  name: string;
  description: string | null;
  keyHash: string;
  oldKeyHash: string | null;
  oldKeyExpiresAt: Date | null;
  scopes: ApiKeyScope[];
  isActive: boolean;
  expiresAt: Date | null;
  overlapSeconds: number;
  maxUses: number | null;
  useCount: number;
  createdAt: Date;
  updatedAt: Date;
  createdBy: string;
}

export interface ApiKeySummary {
  id: string;
  name: string;
  description: string | null;
  scopes: ApiKeyScope[];
  isActive: boolean;
  expiresAt: Date | null;
  overlapSeconds: number;
  useCount: number;
  maxUses: number | null;
  createdAt: Date;
  oldKeyExpiresAt: Date | null;
  status: 'active' | 'expired' | 'revoked' | 'rotation_pending';
}

export interface CreateApiKeyResult {
  id: string;
  /** Raw key — shown once; never stored in plaintext. */
  key: string;
  name: string;
  scopes: ApiKeyScope[];
  expiresAt: Date | null;
}

export interface RotateApiKeyResult {
  oldKeyId: string;
  newKeyId: string;
  /** New raw key — shown once. */
  newKey: string;
  rotatedAt: Date;
  oldKeyExpiresAt: Date;
  overlapSeconds: number;
}

// ---------------------------------------------------------------------------
// Scope validation helpers
// ---------------------------------------------------------------------------

/** Returns true when every scope in `required` is present in `granted`. */
export function hasScopes(granted: ApiKeyScope[], required: ApiKeyScope[]): boolean {
  return required.every((s) => granted.includes(s));
}

/** Validate that all provided scope strings are recognised. */
export function validateScopes(scopes: string[]): ApiKeyScope[] {
  const invalid = scopes.filter((s) => !(API_KEY_SCOPES as readonly string[]).includes(s));
  if (invalid.length > 0) {
    throw new Error(`Unknown API key scopes: ${invalid.join(', ')}`);
  }
  return scopes as ApiKeyScope[];
}

// ---------------------------------------------------------------------------
// Service class
// ---------------------------------------------------------------------------

export class ApiKeyService {
  // ── Creation ──────────────────────────────────────────────────────────────

  /**
   * Create a new scoped API key.
   * Returns the raw key once — it is never retrievable again.
   */
  async createKey(
    userId: string,
    dto: CreateApiKeyDto,
  ): Promise<CreateApiKeyResult> {
    validateScopes(dto.scopes);

    const raw = generateRawApiKey();
    const hash = hashApiKey(raw);
    const overlapSeconds = clampOverlap(dto.overlapSeconds);

    const prisma = getDatabaseClient();
    const record = await prisma.apiKey.create({
      data: {
        name: dto.name,
        description: dto.description ?? null,
        keyHash: hash,
        scopes: dto.scopes as string[],
        isActive: true,
        expiresAt: dto.expiresAt ?? null,
        overlapSeconds,
        maxUses: dto.maxUses ?? null,
        useCount: 0,
        createdBy: userId,
      },
    });

    logger.info('API key created', { keyId: record.id, userId, scopes: dto.scopes });

    return {
      id: record.id,
      key: raw,
      name: record.name,
      scopes: record.scopes as ApiKeyScope[],
      expiresAt: record.expiresAt ?? null,
    };
  }

  // ── Lookup / authentication ───────────────────────────────────────────────

  /**
   * Resolve and validate a raw API key presented in a request.
   * Checks: active, not expired, within max-uses, and respects the rotation
   * overlap window so both old and new keys are accepted during transition.
   */
  async resolveKey(rawKey: string): Promise<ApiKeyRecord | null> {
    const hash = hashApiKey(rawKey);
    const prisma = getDatabaseClient();
    const now = new Date();

    // Look for an exact current-key match first.
    let record = await prisma.apiKey.findFirst({
      where: { keyHash: hash, isActive: true },
    });

    // Fall back: the hash might match an old key still within its overlap window.
    if (!record) {
      record = await prisma.apiKey.findFirst({
        where: {
          oldKeyHash: hash,
          isActive: true,
          oldKeyExpiresAt: { gt: now },
        },
      });
    }

    if (!record) return null;

    // Hard-expiry check on the key itself.
    if (record.expiresAt && record.expiresAt <= now) {
      logger.warn('API key rejected — expired', { keyId: record.id });
      return null;
    }

    // Max-uses guard.
    if (record.maxUses !== null && record.useCount >= record.maxUses) {
      logger.warn('API key rejected — max uses reached', { keyId: record.id });
      return null;
    }

    return record as unknown as ApiKeyRecord;
  }

  /**
   * Increment use-count and stamp last-used timestamp.
   * Call this after a request is authenticated and scope-checked.
   */
  async recordUsage(keyId: string): Promise<void> {
    const prisma = getDatabaseClient();
    await prisma.apiKey.update({
      where: { id: keyId },
      data: {
        useCount: { increment: 1 },
        updatedAt: new Date(),
      },
    });
  }

  // ── Listing ───────────────────────────────────────────────────────────────

  async listKeys(userId: string): Promise<ApiKeySummary[]> {
    const prisma = getDatabaseClient();
    const records = await prisma.apiKey.findMany({
      where: { createdBy: userId },
      orderBy: { createdAt: 'desc' },
    });

    return records.map((r) => this.toSummary(r));
  }

  async getKey(keyId: string, userId: string): Promise<ApiKeySummary | null> {
    const prisma = getDatabaseClient();
    const record = await prisma.apiKey.findFirst({
      where: { id: keyId, createdBy: userId },
    });
    return record ? this.toSummary(record) : null;
  }

  // ── Revocation ────────────────────────────────────────────────────────────

  async revokeKey(keyId: string, userId: string): Promise<void> {
    const prisma = getDatabaseClient();
    const record = await prisma.apiKey.findFirst({
      where: { id: keyId, createdBy: userId },
    });

    if (!record) throw new Error('API key not found');
    if (!record.isActive) throw new Error('API key already revoked');

    await prisma.apiKey.update({
      where: { id: keyId },
      data: { isActive: false, updatedAt: new Date() },
    });

    logger.info('API key revoked', { keyId, userId });
  }

  // ── Rotation ──────────────────────────────────────────────────────────────

  /**
   * Zero-downtime rotation (Issue #1420).
   *
   * Generates a fresh key and stamps the old key hash + expiry on the record.
   * Both keys are accepted until `oldKeyExpiresAt` lapses; the background job
   * then clears the old hash and sets `isActive = false` for expired rows.
   */
  async rotateKey(
    keyId: string,
    userId: string,
    dto: RotateApiKeyDto = {},
  ): Promise<RotateApiKeyResult> {
    const prisma = getDatabaseClient();
    const record = await prisma.apiKey.findFirst({
      where: { id: keyId, createdBy: userId },
    });

    if (!record) throw new Error('API key not found');
    if (!record.isActive) throw new Error('Cannot rotate a revoked key');

    const overlapSeconds = clampOverlap(dto.overlapSeconds ?? (record as any).overlapSeconds);
    const now = new Date();
    const oldKeyExpiresAt = new Date(now.getTime() + overlapSeconds * 1_000);

    const newRaw = generateRawApiKey();
    const newHash = hashApiKey(newRaw);
    const oldHash = (record as any).keyHash as string;

    // Update in-place: new primary hash, old hash kept for overlap window.
    const updated = await prisma.apiKey.update({
      where: { id: keyId },
      data: {
        keyHash: newHash,
        oldKeyHash: oldHash,
        oldKeyExpiresAt,
        overlapSeconds,
        updatedAt: now,
      },
    });

    logger.info('API key rotated', {
      keyId,
      userId,
      overlapSeconds,
      oldKeyExpiresAt: oldKeyExpiresAt.toISOString(),
      reason: dto.reason,
    });

    return {
      oldKeyId: keyId,
      newKeyId: updated.id,
      newKey: newRaw,
      rotatedAt: now,
      oldKeyExpiresAt,
      overlapSeconds,
    };
  }

  // ── Background maintenance ────────────────────────────────────────────────

  /**
   * Expire old-key hashes whose overlap window has elapsed.
   * Safe to call from a cron job or a startup task.
   */
  async expireOldKeys(): Promise<number> {
    const prisma = getDatabaseClient();
    const { count } = await prisma.apiKey.updateMany({
      where: {
        oldKeyHash: { not: null },
        oldKeyExpiresAt: { lte: new Date() },
        isActive: true,
      },
      data: {
        oldKeyHash: null,
        oldKeyExpiresAt: null,
        updatedAt: new Date(),
      },
    });

    if (count > 0) {
      logger.info('Expired old API key hashes after rotation overlap', { count });
    }

    return count;
  }

  // ── Private helpers ───────────────────────────────────────────────────────

  private toSummary(record: any): ApiKeySummary {
    const now = new Date();
    let status: ApiKeySummary['status'] = 'active';

    if (!record.isActive) {
      status = 'revoked';
    } else if (record.expiresAt && record.expiresAt <= now) {
      status = 'expired';
    } else if (record.oldKeyHash && record.oldKeyExpiresAt && record.oldKeyExpiresAt > now) {
      status = 'rotation_pending';
    }

    return {
      id: record.id,
      name: record.name,
      description: record.description,
      scopes: record.scopes as ApiKeyScope[],
      isActive: record.isActive,
      expiresAt: record.expiresAt ?? null,
      overlapSeconds: record.overlapSeconds ?? DEFAULT_OVERLAP_SECONDS,
      useCount: record.useCount ?? 0,
      maxUses: record.maxUses ?? null,
      createdAt: record.createdAt,
      oldKeyExpiresAt: record.oldKeyExpiresAt ?? null,
      status,
    };
  }
}

export const apiKeyService = new ApiKeyService();
