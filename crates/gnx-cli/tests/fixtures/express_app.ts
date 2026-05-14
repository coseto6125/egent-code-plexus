import express from "express";

const app = express();

function loginHandler(req: any, res: any) {
    res.send("ok");
}

function logoutHandler(req: any, res: any) {
    res.send("bye");
}

app.get("/login", loginHandler);
app.post("/logout", logoutHandler);
