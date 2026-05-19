from flask import Flask
app = Flask(__name__)

@app.route("/api/users", methods=["POST"])
def create_user():
    return ""

@app.route("/api/users/<id>")
def get_user(id):
    return ""
