package registry

import (
	"path/filepath"
	"testing"
)

func TestAuthenticateBindsThenEnforces(t *testing.T) {
	r, err := New(filepath.Join(t.TempDir(), "state.json"))
	if err != nil {
		t.Fatal(err)
	}
	if err := r.Authenticate("abcd1234efgh5678", "secret-one"); err != nil {
		t.Fatalf("first bind should succeed: %v", err)
	}
	if err := r.Authenticate("abcd1234efgh5678", "secret-one"); err != nil {
		t.Fatalf("same secret should re-authenticate: %v", err)
	}
	if err := r.Authenticate("abcd1234efgh5678", "secret-two"); err == nil {
		t.Fatal("mismatched secret must be rejected")
	}
	if err := r.Authenticate("abcd1234efgh5678", ""); err == nil {
		t.Fatal("empty secret must be rejected")
	}
}

func TestAuthenticatePersistsAcrossReload(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.json")
	r1, err := New(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := r1.Authenticate("id0000000000000a", "owner-secret"); err != nil {
		t.Fatal(err)
	}
	r2, err := New(path)
	if err != nil {
		t.Fatalf("reload should read persisted state: %v", err)
	}
	if err := r2.Authenticate("id0000000000000a", "owner-secret"); err != nil {
		t.Fatalf("owner secret should still authenticate after reload: %v", err)
	}
	if err := r2.Authenticate("id0000000000000a", "attacker"); err == nil {
		t.Fatal("persisted secret must be enforced after reload")
	}
}
