PRAGMA foreign_keys=ON;
BEGIN IMMEDIATE;
INSERT INTO conversations(id,title) VALUES(92001,'M03 durable startup QA');
INSERT INTO agent_runs(id,conversation_id,model,mode,status,started_at,finished_at,total_steps,spent_micro_usd,limit_micro_usd)
VALUES(92001,92001,'synthetic-journal-orphan','supervised','running',unixepoch()-60,NULL,0,17,1000);
INSERT INTO agent_steps(run_id,seq,kind,observation,started_at,input_tokens,output_tokens)
SELECT id,1,'model_turn','Synthetic recorded checkpoint; no provider called',started_at,7,2 FROM agent_runs WHERE id=92001;
COMMIT;
