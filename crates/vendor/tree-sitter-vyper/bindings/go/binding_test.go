package tree_sitter_vyper_test

import (
	"testing"

	tree_sitter "github.com/tree-sitter/go-tree-sitter"
	tree_sitter_vyper "github.com/tree-sitter/tree-sitter-vyper/bindings/go"
)

func TestCanLoadGrammar(t *testing.T) {
	language := tree_sitter.NewLanguage(tree_sitter_vyper.Language())
	if language == nil {
		t.Errorf("Error loading Vyper grammar")
	}
}
