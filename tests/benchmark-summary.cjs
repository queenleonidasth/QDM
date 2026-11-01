'use strict';
const assert=require('node:assert/strict');
const {summarize}=require('./idm-qdm-benchmark.cjs');
function passingReport(){
  const report={plannedScenarios:['capped','fast'],order:['IDM','QDM','QDM','IDM'],
    status:'passed',finishedAt:'2026-10-27T00:00:00Z',smokeOnly:false,
    binarySha256:'a'.repeat(64),binarySha256After:'a'.repeat(64),
    idmBinarySha256:'b'.repeat(64),idmBinarySha256After:'b'.repeat(64),runs:[]};
  for(const scenario of report.plannedScenarios){
    for(const application of report.order){
      report.runs.push({scenario,application,status:'passed',dispatchToReadyMs:application==='IDM'?2000:1000,
        throughputMiBps:application==='IDM'?50:100,firstRequestToReadyMs:application==='IDM'?1800:900,
        cpuMs:application==='IDM'?500:800,peakWorkingSet:1024,peakPrivateBytes:2048});
    }
  }
  return report;
}
const results=[];
function test(name,run){run();results.push({name,status:'passed'});}
function noHeadline(report){
  for(const row of summarize(report)){
    assert.equal(row.comparisonEligible,false);
    assert.equal(row.qdmSpeedRatioToIdm,null);
    assert.equal(row.qdmReadyTimeDifferencePercent,null);
  }
}
test('complete balanced series retains correct time and CPU medians',()=>{
  const rows=summarize(passingReport());assert.equal(rows.length,2);
  for(const row of rows){assert.equal(row.comparisonEligible,true);assert.equal(row.qdmSpeedRatioToIdm,2);
    assert.equal(row.qdmReadyTimeDifferencePercent,-50);assert.equal(row.groups.QDM.medianCpuMs,800);
    assert.equal(row.groups.IDM.medianFirstRequestToReadyMs,1800);assert.equal(row.groups.QDM.notRun,0);}
});
test('one timeout suppresses every headline and preserves failure counts',()=>{
  const report=passingReport();report.status='failed';report.runs[6].status='failed';noHeadline(report);
  const row=summarize(report)[1];assert.equal(row.groups.QDM.failedRuns,1);assert.equal(row.groups.QDM.successfulRuns,1);
  assert.equal(row.groups.QDM.plannedRuns,2);assert.equal(row.groups.QDM.medianDispatchToReadyMs,1000);
});
test('unattempted runs and entire scenarios remain visible',()=>{
  const report=passingReport();report.runs=report.runs.slice(0,3);report.status='failed';noHeadline(report);
  const rows=summarize(report);assert.equal(rows[0].groups.IDM.notRun,1);
  assert.equal(rows[1].groups.IDM.notRun,2);assert.equal(rows[1].groups.QDM.notRun,2);
  assert.equal(rows[1].groups.QDM.medianCpuMs,null);
});
test('running rows and absent finalization cannot certify a comparison',()=>{
  const report=passingReport();delete report.finishedAt;report.runs[7].status='running';noHeadline(report);
  assert.equal(summarize(report)[1].groups.IDM.unfinishedRuns,1);
});
test('unbalanced order is descriptive even if every attempt passes',()=>{
  const report=passingReport();report.order=['IDM','QDM','QDM'];
  report.runs=report.runs.filter((_row,index)=>index%4!==3);noHeadline(report);
});
test('a single pair cannot become a headline comparison',()=>{
  const report=passingReport();report.order=['IDM','QDM'];
  report.runs=report.runs.filter((_row,index)=>index%4<2);noHeadline(report);
});
test('wrong observed order is rejected despite matching counts',()=>{
  const report=passingReport();[report.runs[0],report.runs[1]]=[report.runs[1],report.runs[0]];noHeadline(report);
});
test('extra attempts cannot silently replace failed planned samples',()=>{
  const report=passingReport();report.runs.push({...report.runs[7]});noHeadline(report);
  assert.equal(summarize(report)[1].groups.IDM.unexpectedRuns,1);
});
test('changed or missing final executable hashes suppress comparison',()=>{
  for(const key of ['binarySha256After','idmBinarySha256After']){
    const report=passingReport();report[key]='c'.repeat(64);noHeadline(report);
    delete report[key];noHeadline(report);
  }
});
test('old reports without an explicit plan stay descriptive',()=>{
  const report=passingReport();delete report.plannedScenarios;noHeadline(report);
});
test('smoke runs remain descriptive',()=>{const report=passingReport();report.smokeOnly=true;noHeadline(report);});
test('public-origin unknown first-request timing stays null and outliers stay visible',()=>{
  const report=passingReport();for(const row of report.runs)row.firstRequestToReadyMs=null;
  report.runs[2].dispatchToReadyMs=21000;
  const row=summarize(report)[0];assert.equal(row.groups.QDM.medianFirstRequestToReadyMs,null);
  assert.deepEqual(row.groups.QDM.dispatchToReadyRangeMs,{min:1000,max:21000});
  assert.equal(row.groups.QDM.medianDispatchToReadyMs,11000);
});
test('missing or invalid elapsed time cannot certify a comparison',()=>{
  for(const value of [undefined,null,0,NaN]){const report=passingReport();report.runs[0].dispatchToReadyMs=value;noHeadline(report);}
});
console.log(JSON.stringify({status:'passed',passed:results.length,failed:0,cases:results},null,2));
