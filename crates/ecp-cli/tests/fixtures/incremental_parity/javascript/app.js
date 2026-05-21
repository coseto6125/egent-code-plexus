const express = require("express");

function createApp() {
  const app = express();
  app.use(express.json());
  return app;
}

function startServer(port) {
  const app = createApp();
  app.listen(port, () => {
    console.log(`Server running on port ${port}`);
  });
}

module.exports = { createApp, startServer };
