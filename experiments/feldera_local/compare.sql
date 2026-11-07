WITH source_totals AS (
    SELECT group_key, COUNT(*) AS row_count, SUM(amount)::BIGINT AS total_amount
    FROM public.events
    GROUP BY group_key
), missing_from_sink AS (
    SELECT group_key, row_count, total_amount FROM source_totals
    EXCEPT
    SELECT group_key, row_count, total_amount FROM public.feldera_group_totals
), extra_in_sink AS (
    SELECT group_key, row_count, total_amount FROM public.feldera_group_totals
    EXCEPT
    SELECT group_key, row_count, total_amount FROM source_totals
)
SELECT NOT EXISTS (SELECT 1 FROM missing_from_sink)
   AND NOT EXISTS (SELECT 1 FROM extra_in_sink) AS sink_matches_source;
