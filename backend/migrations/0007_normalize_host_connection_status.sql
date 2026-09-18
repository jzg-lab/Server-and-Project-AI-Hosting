PRAGMA foreign_keys = ON;

-- Docker discovery is optional and must not leave a reachable host looking
-- disconnected after a previous discovery run.
UPDATE hosts
SET status = 'connection_ready'
WHERE status IN ('docker_unavailable', 'docker_permission_denied');
