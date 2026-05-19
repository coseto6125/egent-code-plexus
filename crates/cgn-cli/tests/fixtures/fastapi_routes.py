from fastapi import FastAPI, APIRouter

app = FastAPI()
router = APIRouter()

@app.get("/users/{id}")
def read_user(id: int):
    return {"id": id}

@app.post("/items")
async def create_item(item: dict):
    return item

@router.delete("/items/{id}")
def delete_item(id: int):
    return {"deleted": id}

@router.patch("/items/{id}")
def patch_item(id: int):
    return {"patched": id}

# 對照組：非 HTTP method decorator，不該被抓
@app.middleware("http")
async def middleware_fn(request, call_next):
    return await call_next(request)
