package demo;
import io.grpc.ServerBuilder;
import user.UserServiceGrpc;

public class GrpcServer {
    public static void main(String[] args) {
        ServerBuilder.forPort(50051)
            .addService(new UserServiceGrpc.UserServiceImplBase() {})
            .build();
    }
}
