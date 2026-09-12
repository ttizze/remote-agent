-- Chromium cookie database v24, isolated synthetic values only.
-- Ciphertext: Python hashlib.pbkdf2_hmac + OpenSSL AES-128-CBC, password fixture-password.
CREATE TABLE meta(key LONGVARCHAR NOT NULL UNIQUE PRIMARY KEY, value LONGVARCHAR);
INSERT INTO meta VALUES('version', '24'), ('last_compatible_version', '24');
CREATE TABLE cookies(
    creation_utc INTEGER NOT NULL, host_key TEXT NOT NULL, top_frame_site_key TEXT NOT NULL,
    name TEXT NOT NULL, value TEXT NOT NULL, encrypted_value BLOB NOT NULL,
    path TEXT NOT NULL, expires_utc INTEGER NOT NULL, is_secure INTEGER NOT NULL,
    is_httponly INTEGER NOT NULL, last_access_utc INTEGER NOT NULL, has_expires INTEGER NOT NULL,
    is_persistent INTEGER NOT NULL, priority INTEGER NOT NULL, samesite INTEGER NOT NULL,
    source_scheme INTEGER NOT NULL, source_port INTEGER NOT NULL, last_update_utc INTEGER NOT NULL,
    source_type INTEGER NOT NULL, has_cross_site_ancestor INTEGER NOT NULL
);
CREATE UNIQUE INDEX cookies_unique_index ON cookies(
    host_key, top_frame_site_key, has_cross_site_ancestor, name, path, source_scheme, source_port
);
INSERT INTO cookies VALUES (
    13433673600000000, '.example.test', '', 'login', '',
    X'7631301ed2c90284df3c92295d12077eddf3a39d9f5e797292a851f735783b36e7d42c05d07aaf4e74c13c70d6f76fa7e7a739',
    '/account', 13444473600000000, 1, 1, 13433673600000000, 1, 1, 1, 1, 2, 443,
    13433673600000000, 1, 0
);
