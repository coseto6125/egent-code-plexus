import grpc
import user_pb2_grpc

class UserServicer(user_pb2_grpc.UserServiceServicer):
    pass

def serve():
    server = grpc.server(None)
    user_pb2_grpc.add_UserServiceServicer_to_server(UserServicer(), server)
