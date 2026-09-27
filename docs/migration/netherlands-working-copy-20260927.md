# Netherlands working-copy reconciliation (2026-09-27)

The Netherlands `custom` working copy was first committed as
`2540e7e1ada8fa33230ba9d28acbf7f2a042d671`, based on
`abc0c284bbfba65252df2a368e1cb723fbef3983`, before repository migration.
This saved commit is retained as a parent of the merge into the latest `custom` branch.

The merge preserves the PostgreSQL/MySQL/application `TZ: Europe/Amsterdam`
settings from the Netherlands Compose file and its additional deployment-history
entries, without replacing newer application code.

`.env` backups, build caches and the downloaded `docs/sub2api-report.html` forum
page are excluded. The forum page contains a token-like value and is not application source.
No real environment credentials, database backups or running-state files were selected.

This is a source-control operation only. The Compose changes were already present
on the Netherlands machine; no Compose lifecycle command or database timezone
change was run. The Netherlands checkout remains at its saved commit.
