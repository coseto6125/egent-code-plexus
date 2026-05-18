import express from "express";
const app = express();
app.post("/api/users", (req, res) => res.json({}));
app.get("/api/users/:id", (req, res) => res.json({}));
