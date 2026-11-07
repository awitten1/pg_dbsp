-- This table is populated from PostgreSQL logical replication, not HTTP ingress.
-- Try inserts, updates, and deletes in PostgreSQL while the pipeline runs.
CREATE TABLE events (
    id BIGINT NOT NULL PRIMARY KEY,
    group_key VARCHAR NOT NULL,
    amount BIGINT NOT NULL
) WITH (
    'connectors' = '[{
        "name": "postgres_events_cdc",
        "transport": {
            "name": "postgres_cdc_input",
            "config": {
                "uri": "postgres://__PG_USER__@127.0.0.1:55434/postgres",
                "publication": "feldera_events",
                "source_table": "public.events"
            }
        }
    }]'
);

CREATE MATERIALIZED VIEW group_totals WITH (
    'connectors' = '[{
        "name": "postgres_group_totals_output",
        "index": "group_totals_idx",
        "send_snapshot": true,
        "transport": {
            "name": "postgres_output",
            "config": {
                "uri": "postgres://__PG_USER__@127.0.0.1:55434/postgres",
                "table": "feldera_group_totals",
                "mode": "materialized"
            }
        }
    }]'
) AS
SELECT group_key, COUNT(*) AS row_count, SUM(amount) AS total_amount
FROM events
GROUP BY group_key;

CREATE INDEX group_totals_idx ON group_totals(group_key);
