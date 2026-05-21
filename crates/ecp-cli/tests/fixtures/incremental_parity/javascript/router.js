function Router() {
  this.routes = {};
}

Router.prototype.get = function (path, handler) {
  this.routes[`GET:${path}`] = handler;
};

Router.prototype.post = function (path, handler) {
  this.routes[`POST:${path}`] = handler;
};

Router.prototype.dispatch = function (method, path, req, res) {
  const key = `${method}:${path}`;
  const handler = this.routes[key];
  if (handler) return handler(req, res);
  res.status(404).send("Not found");
};

function createRouter() {
  return new Router();
}

module.exports = { Router, createRouter };
