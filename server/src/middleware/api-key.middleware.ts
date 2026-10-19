/**
 * API Key Authentication & Scope Enforcement Middleware — Issue #1420
 *
 * Extracts the raw API key from the `X-API-Key` header (or `api_key` query
 * param as a fallback), resolves it against the database, and attaches the
 * resolved record to `req.apiKey`.
 *
 * `requireApiKeyScope(...scopes)` enforces per-request scope checks and
 * returns HTTP 403 when the key lacks any required scope.
 */

import { NextFunction, Request, Response } from 'express';
import { ApiKeyRecord, ApiKeyScope, apiKeyService } from '../services/api-key.service';
import { HttpError } from '../utils/http-error';
import { logger } from '../services/logger.service';

// ---------------------------------------------------------------------------
// Augment Express request with resolved API key
// ---------------------------------------------------------------------------

declare global {
  namespace Express {
    interface Request {
      apiKey?: ApiKeyRecord;
    }
  }
}

// ---------------------------------------------------------------------------
// Authentication middleware
// ---------------------------------------------------------------------------

/**
 * Authenticate an inbound request using an API key.
 *
 * The key is extracted from:
 *   1. `X-API-Key` request header (preferred)
 *   2. `api_key` query-string parameter (fallback)
 *
 * On success `req.apiKey` is populated and the use-count is incremented.
 * On failure the request is rejected with HTTP 401.
 */
export const authenticateApiKey = async (
  req: Request,
  _res: Response,
  next: NextFunction,
): Promise<void> => {
  const raw =
    (req.headers['x-api-key'] as string | undefined) ??
    (req.query['api_key'] as string | undefined);

  if (!raw || raw.trim() === '') {
    return next(new HttpError(401, 'API key required'));
  }

  try {
    const record = await apiKeyService.resolveKey(raw.trim());

    if (!record) {
      logger.warn('API key authentication failed — key not found or expired', {
        ip: req.ip,
        path: req.originalUrl,
      });
      return next(new HttpError(401, 'Invalid or expired API key'));
    }

    req.apiKey = record;

    // Fire-and-forget usage recording — never block the request.
    apiKeyService.recordUsage(record.id).catch((err) =>
      logger.error('Failed to record API key usage', { keyId: record.id, error: err }),
    );

    return next();
  } catch (err) {
    logger.error('API key middleware error', { error: err, path: req.originalUrl });
    return next(new HttpError(500, 'Internal server error during API key validation'));
  }
};

// ---------------------------------------------------------------------------
// Scope enforcement middleware factory
// ---------------------------------------------------------------------------

/**
 * Enforce that the authenticated API key holds **all** of the listed scopes.
 *
 * Must be placed after `authenticateApiKey` in the middleware chain.
 *
 * Returns HTTP 401 when no key has been resolved yet.
 * Returns HTTP 403 when the key is missing one or more required scopes.
 *
 * @example
 * router.post(
 *   '/results',
 *   authenticateApiKey,
 *   requireApiKeyScope('write:results'),
 *   handler,
 * );
 */
export const requireApiKeyScope =
  (...requiredScopes: ApiKeyScope[]) =>
  (req: Request, _res: Response, next: NextFunction): void => {
    if (!req.apiKey) {
      return next(new HttpError(401, 'API key authentication required'));
    }

    const granted = req.apiKey.scopes;
    const missing = requiredScopes.filter((s) => !granted.includes(s));

    if (missing.length > 0) {
      logger.warn('API key scope check failed', {
        keyId: req.apiKey.id,
        required: requiredScopes,
        granted,
        missing,
        path: req.originalUrl,
      });
      return next(
        new HttpError(
          403,
          `Forbidden — API key missing required scopes: ${missing.join(', ')}`,
        ),
      );
    }

    return next();
  };

/**
 * Enforce that the authenticated API key holds **at least one** of the listed
 * scopes (OR semantics, as opposed to `requireApiKeyScope`'s AND semantics).
 */
export const requireAnyApiKeyScope =
  (...anyScopes: ApiKeyScope[]) =>
  (req: Request, _res: Response, next: NextFunction): void => {
    if (!req.apiKey) {
      return next(new HttpError(401, 'API key authentication required'));
    }

    const granted = req.apiKey.scopes;
    const hasAny = anyScopes.some((s) => granted.includes(s));

    if (!hasAny) {
      logger.warn('API key scope check failed (any)', {
        keyId: req.apiKey.id,
        required: anyScopes,
        granted,
        path: req.originalUrl,
      });
      return next(
        new HttpError(
          403,
          `Forbidden — API key requires at least one of: ${anyScopes.join(', ')}`,
        ),
      );
    }

    return next();
  };
