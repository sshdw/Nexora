-- Synthetic isolated QA only; close the app before seeding.
PRAGMA foreign_keys=ON;
BEGIN IMMEDIATE;
INSERT INTO conversations(id,title) VALUES(92001,'M03 final durable startup QA');
INSERT INTO permission_rules(id,preset,tool_pattern,path_pattern,effect) VALUES(92011,'*','read_file','synthetic.txt','allow');
INSERT INTO agent_runs(id,conversation_id,model,mode,status,started_at,finished_at,total_steps,spent_micro_usd,limit_micro_usd) VALUES(92001,92001,'synthetic-journal-orphan','supervised','running',unixepoch()-60,NULL,0,17,1000);
INSERT INTO agent_steps(run_id,seq,kind,tool_name,arguments,observation,status,started_at,rule_id,decided_by,input_tokens,output_tokens) SELECT id,1,'tool_call','read_file','{"path":"synthetic.txt"}','Synthetic retained checkpoint; no tool called','succeeded',started_at,92011,'rule',7,2 FROM agent_runs WHERE id=92001;
COMMIT;
-- Required compatibility: this clears only the recorded step rule_id.
DELETE FROM permission_rules WHERE id=92011;

UPDATE agent_runs SET status='completed', finished_at=unixepoch(),total_steps=1 WHERE id=92001;
