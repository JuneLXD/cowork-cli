# Feedback table

`feedback.sql` creates the `public.feedback` table that `room feedback bug|advice` writes to.
Apply it once per Supabase project, either in the dashboard SQL editor or through the
Management API with a personal access token:

```sh
curl -X POST "https://api.supabase.com/v1/projects/<project ref>/database/query" \
  -H "Authorization: Bearer <access token>" -H "Content-Type: application/json" \
  -d "$(python3 -c 'import json,sys; print(json.dumps({"query": sys.stdin.read()}))' < supabase/feedback.sql)"
```

The table is insert-only for the `anon` role, so the publishable key that ships in the
binary can add reports but never read, change, or delete them. Reading reports needs the
dashboard or a server-side key, which must never be put in the CLI or committed.
