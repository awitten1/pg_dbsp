CREATE TABLE IF NOT EXISTS public.events (
    id BIGINT PRIMARY KEY,
    group_key TEXT NOT NULL,
    amount BIGINT NOT NULL
);

ALTER TABLE public.events REPLICA IDENTITY FULL;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_publication WHERE pubname = 'feldera_events') THEN
        CREATE PUBLICATION feldera_events FOR TABLE public.events;
    END IF;
END
$$;

INSERT INTO public.events (id, group_key, amount) VALUES
    (1, 'east', 10),
    (2, 'east', 20),
    (3, 'west', 7)
ON CONFLICT (id) DO NOTHING;
