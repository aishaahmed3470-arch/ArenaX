/**
 * API Key Management Routes — Issue #1420
 *
 * POST   /api/api-keys              — create a new scoped API key
 * GET    /api/api-keys              — list all API keys for the caller
 * GET    /api/api-keys/:id          — get details for a specific key
 * DELETE /api/api-keys/:id          — revoke a key
 * POST   /api/api-keys/:id/rotate   — rotate a key (zero-downtime)
 *
 * All management endpoints require JWT authentication (existing middleware).
 * The public demo endpoint at the bottom shows read/write scope enforcement.
 */

import { Router, Request, Response, NextFunction } from 'express';
import { authenticateJWT } from '../middleware/auth.middleware';
import {
  authenticateApiKey,
  requireApiKeyScope,
} from '../middleware/api-key.middleware';
import {
  apiKeyService,
  validateScopes,
  ApiKeyScope,
  CreateApiKeyDto,
  RotateApiKeyDto,
} from '../services/api-key.service';
import { HttpError } from '../utils/http-error';
import { logger } from '../services/logger.service';

const router = Router();

// ---------------------------------------------------------------------------
// Management endpoints (JWT-authenticated, must be the key owner)
// ---------------------------------------------------------------------------

/**
 * POST /api/api-keys
 * Create a new API key with the specified scopes.
 *
 * Body: { name, description?, scopes, expiresAt?, overlapSeconds?, maxUses? }
 */
router.post(
  '/',
  authenticateJWT,
  async (req: Request, res: Response, next: NextFunction) => {
    try {
      const { name, description, scopes, expiresAt, overlapSeconds, maxUses } =
        req.body as {
          name: string;
          description?: string;
          scopes: string[];
          expiresAt?: string;
          overlapSeconds?: number;
          maxUses?: number;
        };

      if (!name || typeof name !== 'string' || name.trim() === '') {
        return next(new HttpError(400, 'name is required'));
      }
      if (!Array.isArray(scopes) || scopes.length === 0) {
        return next(new HttpError(400, 'scopes must be a non-empty array'));
      }

      const validatedScopes = validateScopes(scopes);

      const dto: CreateApiKeyDto = {
        name: name.trim(),
        description,
        scopes: validatedScopes,
        expiresAt: expiresAt ? new Date(expiresAt) : undefined,
        overlapSeconds,
        maxUses,
      };

      const result = await apiKeyService.createKey(req.user!.id, dto);

      logger.info('API key created via route', {
        keyId: result.id,
        userId: req.user!.id,
        scopes: result.scopes,
      });

      // 201 with the raw key — shown once only.
      return res.status(201).json({
        message: 'API key created. Store the key securely — it will not be shown again.',
        ...result,
      });
    } catch (err: any) {
      if (err.message?.startsWith('Unknown API key scopes')) {
        return next(new HttpError(400, err.message));
      }
      return next(err);
    }
  },
);

/**
 * GET /api/api-keys
 * List all API keys owned by the authenticated user.
 */
router.get(
  '/',
  authenticateJWT,
  async (req: Request, res: Response, next: NextFunction) => {
    try {
      const keys = await apiKeyService.listKeys(req.user!.id);
      return res.json({ apiKeys: keys, total: keys.length });
    } catch (err) {
      return next(err);
    }
  },
);

/**
 * GET /api/api-keys/:id
 * Get details for a specific API key (no raw key exposed).
 */
router.get(
  '/:id',
  authenticateJWT,
  async (req: Request, res: Response, next: NextFunction) => {
    try {
      const key = await apiKeyService.getKey(req.params.id, req.user!.id);
      if (!key) return next(new HttpError(404, 'API key not found'));
      return res.json(key);
    } catch (err) {
      return next(err);
    }
  },
);

/**
 * DELETE /api/api-keys/:id
 * Revoke an API key immediately.
 */
router.delete(
  '/:id',
  authenticateJWT,
  async (req: Request, res: Response, next: NextFunction) => {
    try {
      await apiKeyService.revokeKey(req.params.id, req.user!.id);
      return res.json({ message: 'API key revoked successfully' });
    } catch (err: any) {
      if (err.message === 'API key not found') return next(new HttpError(404, err.message));
      if (err.message === 'API key already revoked') return next(new HttpError(409, err.message));
      return next(err);
    }
  },
);

/**
 * POST /api/api-keys/:id/rotate
 * Rotate an API key with a configurable overlap period.
 *
 * Body (optional): { reason?, overlapSeconds? }
 */
router.post(
  '/:id/rotate',
  authenticateJWT,
  async (req: Request, res: Response, next: NextFunction) => {
    try {
      const dto: RotateApiKeyDto = {
        reason: req.body?.reason,
        overlapSeconds: req.body?.overlapSeconds,
      };

      const result = await apiKeyService.rotateKey(req.params.id, req.user!.id, dto);

      return res.json({
        message:
          'API key rotated. The new key is shown once only. ' +
          `The old key remains valid until ${result.oldKeyExpiresAt.toISOString()}.`,
        ...result,
      });
    } catch (err: any) {
      if (err.message === 'API key not found') return next(new HttpError(404, err.message));
      if (err.message === 'Cannot rotate a revoked key')
        return next(new HttpError(409, err.message));
      return next(err);
    }
  },
);

// ---------------------------------------------------------------------------
// Scope-enforced resource endpoints (API-key authenticated)
// ---------------------------------------------------------------------------

/**
 * GET /api/api-keys/access/tournaments
 * Read-only tournament listing — requires `read:tournaments`.
 * Demonstrates that a read-only key can call this endpoint.
 */
router.get(
  '/access/tournaments',
  authenticateApiKey,
  requireApiKeyScope('read:tournaments'),
  (_req: Request, res: Response) => {
    res.json({ tournaments: [], message: 'Tournaments list (read:tournaments scope verified)' });
  },
);

/**
 * POST /api/api-keys/access/results
 * Write a match result — requires `write:results`.
 * A read-only key (no write:results scope) receives HTTP 403.
 */
router.post(
  '/access/results',
  authenticateApiKey,
  requireApiKeyScope('write:results'),
  (_req: Request, res: Response) => {
    res.status(201).json({ message: 'Result recorded (write:results scope verified)' });
  },
);

export default router;
