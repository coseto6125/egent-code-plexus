import { Server } from "@grpc/grpc-js";
import { user_proto } from "./pb";

const server = new Server();
server.addService(user_proto.UserService.service, {});
