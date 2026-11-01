'use strict';
// Node 24 built-in SQLite reader for isolated QDM test profiles only.
const path=require('node:path');
const { DatabaseSync }=require('node:sqlite');
function openState(dataDir) {
  const db=new DatabaseSync(path.join(dataDir, 'state-v3.sqlite3'), {readOnly:true});
  try {
    const settings=db.prepare('SELECT value_json FROM app_settings WHERE id=1').get();
    if(!settings)throw new Error('Missing SQLite settings');
    const tasks=db.prepare('SELECT id,task_json FROM downloads ORDER BY ordinal').all().map(row=>{
      const task=JSON.parse(Buffer.from(row.task_json).toString('utf8'));
      task.parts=db.prepare('SELECT part_json FROM checkpoints WHERE download_id=? ORDER BY start_byte')
        .all(row.id).map(part=>JSON.parse(Buffer.from(part.part_json).toString('utf8')));
      if(task.view.id!==row.id)throw new Error('SQLite job identity mismatch');
      return task;
    });
    return {schema_version:3,settings:JSON.parse(Buffer.from(settings.value_json).toString('utf8')),tasks};
  }finally{ db.close(); }
}
function journalMode(dataDir) {
  const db=new DatabaseSync(path.join(dataDir,'state-v3.sqlite3'),{readOnly:true});
  try{return db.prepare('PRAGMA journal_mode').get().journal_mode;}finally{db.close();}
}
module.exports={openState,journalMode};
