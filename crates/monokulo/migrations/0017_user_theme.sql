-- Per-user light/dark theme preference (design-language rollout follow-up:
-- a no-JS toggle, persisted server-side rather than a cookie/localStorage,
-- so it's consistent across devices and doesn't depend on client storage
-- surviving). `'system'` (the default, and existing rows' backfilled value)
-- means "no explicit choice - follow the browser's own `prefers-color-
-- scheme`"; `'light'`/`'dark'` mean the user explicitly overrode it via the
-- nav's theme-toggle form, which always wins over the OS preference either
-- way (`_styles.html.hbs`'s `:root[data-theme="dark"]` block).
ALTER TABLE users ADD COLUMN theme TEXT NOT NULL DEFAULT 'system';
