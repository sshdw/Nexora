-- Synthetic QA only. Run once against the same isolated, app-initialized DB
-- after seed.sql, while every Nexora process using that DB is closed.
PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;
INSERT INTO conversations (id, title) VALUES (91007, 'P01 interrupted-only QA');
INSERT INTO messages (id, conversation_id, role, content, created_at)
VALUES (91007, 91007, 'user', 'Synthetic retained-history validation. No execution requested.', unixepoch() - 1000);
INSERT INTO agent_runs
    (id, conversation_id, model, mode, status, started_at, finished_at,
     total_steps, final_content, error, spent_micro_usd, limit_micro_usd)
VALUES
    (91007, 91007, 'qa-single-orphan', 'supervised', 'running', unixepoch()-60, NULL, 0, NULL, NULL, 150, 900);
INSERT INTO agent_steps (run_id, seq, kind, observation, started_at, input_tokens, output_tokens)
SELECT id, 1, 'model_turn', 'Synthetic retained reasoning', started_at, 12, 3
FROM agent_runs WHERE id = 91007;
INSERT INTO agent_steps
    (run_id, seq, kind, tool_name, arguments, observation, status, started_at, duration_ms)
SELECT id, 2, 'tool_call', 'read_file', '{"path":"synthetic.txt"}',
       'Synthetic retained file body', 'succeeded', started_at+1, 5
FROM agent_runs WHERE id = 91007;
COMMIT;
