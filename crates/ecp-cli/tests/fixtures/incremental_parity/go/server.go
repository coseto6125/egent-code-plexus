package main

import (
	"fmt"
	"net/http"
)

type Server struct {
	addr string
	mux  *http.ServeMux
}

func NewServer(addr string) *Server {
	return &Server{addr: addr, mux: http.NewServeMux()}
}

func (s *Server) Handle(pattern string, handler http.HandlerFunc) {
	s.mux.HandleFunc(pattern, handler)
}

func (s *Server) Start() error {
	fmt.Printf("Listening on %s\n", s.addr)
	return http.ListenAndServe(s.addr, s.mux)
}
