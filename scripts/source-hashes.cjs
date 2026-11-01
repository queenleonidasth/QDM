const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
let files=['Cargo.toml','Cargo.lock','build.rs','.cargo/config.toml'];
for(const dir of ['src','resources'])for(const name of fs.readdirSync(dir,{recursive:true})){const p=path.join(dir,name);if(fs.statSync(p).isFile())files.push(p)}
const hashes={};for(const file of files)hashes[file.split(path.sep).join('/')]=crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const value={schema:1,version:'0.2.0',rust:'1.99.1',target:'x86_64-pc-windows-msvc',binarySha256:crypto.createHash('sha256').update(fs.readFileSync('target/release/QDM.exe')).digest('hex'),files:hashes};
fs.writeFileSync('docs/evidence/source-hashes.json',JSON.stringify(value,null,2)+'\n');console.log(JSON.stringify({binarySha256:value.binarySha256,sourceFiles:files.length}));
