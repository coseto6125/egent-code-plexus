const LEVELS = { debug: 0, info: 1, warn: 2, error: 3 };

function createLogger(name, level) {
  return {
    log(msg) {
      console.log(`[${name}] ${msg}`);
    },
    warn(msg) {
      console.warn(`[${name}] WARN: ${msg}`);
    },
    error(msg) {
      console.error(`[${name}] ERROR: ${msg}`);
    },
  };
}

function formatMessage(level, msg) {
  return `${new Date().toISOString()} [${level.toUpperCase()}] ${msg}`;
}

module.exports = { createLogger, formatMessage };
