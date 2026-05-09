-- Migration: document the new 'waiting' status value for sessions and issues.
--
-- SQLite stores statuses as plain TEXT so no schema change is required.
-- This migration exists purely to document the new valid values and ensure
-- the migration sequence is applied in order.
--
-- New valid values:
--   sessions.status: 'waiting'  (session paused for human input)
--   issues.status:   'waiting'  (issue paused for human input)
SELECT 1; -- no-op DDL; marker only
