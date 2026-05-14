from fastapi import FastAPI, Depends

app = FastAPI()

def get_db():
    return None

def get_current_user(db = Depends(get_db)):
    return None

@app.get("/users/{id}")
def read_user(id: int, user = Depends(get_current_user)):
    return user
