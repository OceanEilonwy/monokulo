async function captureCoverageStage(target, stage, testInfo) {
  if (process.env.COVERAGE_SCREENSHOTS !== '1') return;
  if (!/^[a-z]+-[a-z0-9-]+$/.test(stage)) throw new Error(`invalid coverage stage: ${stage}`);
  const body = await target.screenshot({ animations: 'disabled', caret: 'hide' });
  await testInfo.attach(`coverage-stage:${stage}`, { body, contentType: 'image/png' });
}

module.exports = { captureCoverageStage };
