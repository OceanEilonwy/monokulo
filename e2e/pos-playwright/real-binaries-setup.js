// Global setup for the real-binaries suite: builds the binaries once. Each
// spec file starts its own processes from them (real-stack.js).
const { buildBinaries } = require('./real-stack');

module.exports = async function globalSetup() {
  buildBinaries();
};
