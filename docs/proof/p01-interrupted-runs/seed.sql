-- Synthetic QA only. Run once against a fresh isolated, app-initialized v15 DB
-- while every Nexora process using that isolated DB is closed.
PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;
INSERT INTO conversations (id, title) VALUES (91001, 'P01 interrupted history QA');
INSERT INTO messages (conversation_id, role, content, created_at)
VALUES (91001, 'user', 'Synthetic retained-history validation. No execution requested.', unixepoch() - 1000);
INSERT INTO agent_runs
    (id, conversation_id, model, mode, status, started_at, finished_at,
     total_steps, final_content, error, spent_micro_usd, limit_micro_usd)
VALUES
    (91001, 91001, 'qa-orphan', 'supervised', 'running', unixepoch()-60, NULL, 0, NULL, NULL, 150, 900),
    (91002, 91001, 'qa-legacy-error', 'supervised', 'error', unixepoch()-120, unixepoch()-119, 2, NULL, 'run interrupted by application shutdown', 150, 900),
    (91003, 91001, 'qa-completed', 'supervised', 'completed', unixepoch()-180, unixepoch()-179, 2, 'Synthetic completed output', NULL, 150, 900),
    (91004, 91001, 'qa-cancelled', 'supervised', 'cancelled', unixepoch()-240, unixepoch()-239, 2, NULL, NULL, 150, 900),
    (91005, 91001, 'qa-budget', 'supervised', 'budget_exhausted', unixepoch()-300, unixepoch()-299, 2, NULL, NULL, 150, 900),
    (91006, 91001, 'qa-spend', 'supervised', 'spend_limit_exceeded', unixepoch()-360, unixepoch()-359, 2, NULL, NULL, 150, 900);
INSERT INTO agent_steps (run_id, seq, kind, observation, started_at, input_tokens, output_tokens)
SELECT id, 1, 'model_turn', 'Synthetic retained reasoning', started_at, 12, 3
FROM agent_runs WHERE conversation_id = 91001;
INSERT INTO agent_steps
    (run_id, seq, kind, tool_name, arguments, observation, status, started_at, duration_ms)
SELECT id, 2, 'tool_call', 'read_file', '{"path":"synthetic.txt"}',
       'Synthetic retained file body', 'succeeded', started_at+1, 5
FROM agent_runs WHERE conversation_id = 91001;
COMMIT;
