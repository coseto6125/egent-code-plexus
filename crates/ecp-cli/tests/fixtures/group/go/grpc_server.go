package main

import (
	"google.golang.org/grpc"
	pb "example/userpb"
)

type userSrv struct {
	pb.UnimplementedUserServiceServer
}

func main() {
	s := grpc.NewServer()
	pb.RegisterUserServiceServer(s, &userSrv{})
}
