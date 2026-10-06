-- Operator-scoped appearance is resolved via verified Host before login.
-- Logos are not exposed until there is a verified public asset upload path.
ALTER TABLE operators
    ADD COLUMN primary_color TEXT NOT NULL DEFAULT '#2563EB'
        CHECK (primary_color ~ '^#[0-9A-Fa-f]{6}$'),
    ADD COLUMN default_locale TEXT NOT NULL DEFAULT 'zh-CN'
        CHECK (default_locale IN ('zh-CN', 'en')),
    ADD COLUMN appearance_revision BIGINT NOT NULL DEFAULT 1
        CHECK (appearance_revision > 0);
