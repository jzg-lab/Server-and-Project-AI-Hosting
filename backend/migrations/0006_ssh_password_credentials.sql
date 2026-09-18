PRAGMA foreign_keys = OFF;

CREATE TABLE secret_ref_descriptors_v6 (
    credential_ref TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('ssh_key', 'ssh_password', 'model_key')),
    idempotency_key TEXT NOT NULL UNIQUE,
    secret_sha256 TEXT NOT NULL,
    response_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

INSERT INTO secret_ref_descriptors_v6(
    credential_ref, kind, idempotency_key, secret_sha256, response_json, created_at
)
SELECT credential_ref, kind, idempotency_key, secret_sha256, response_json, created_at
FROM secret_ref_descriptors;

DROP TABLE secret_ref_descriptors;
ALTER TABLE secret_ref_descriptors_v6 RENAME TO secret_ref_descriptors;

PRAGMA foreign_keys = ON;
