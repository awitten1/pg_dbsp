-- Feldera SQL for the Docker Compose demo.
-- The PostgreSQL service is reachable at postgres:5432.
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
                "uri": "postgres://postgres:feldera-local@postgres:5432/postgres",
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
                "uri": "postgres://postgres:feldera-local@postgres:5432/postgres",
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
