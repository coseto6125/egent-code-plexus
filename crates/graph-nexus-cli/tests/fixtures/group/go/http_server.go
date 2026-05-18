package main

import (
    "net/http"
)

func main() {
    mux := http.NewServeMux()
    mux.HandleFunc("/api/users", createUser)
    mux.HandleFunc("/api/users/{id}", getUser)
    http.ListenAndServe(":8080", mux)
}

func createUser(w http.ResponseWriter, r *http.Request) { _ = r.Method }
func getUser(w http.ResponseWriter, r *http.Request)    { _ = r.Method }
