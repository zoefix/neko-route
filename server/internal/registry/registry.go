// Package registry tracks live tunnel sessions by client ID and persists the
// ID->secret binding so a subdomain can only be reclaimed by its owner.
package registry

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"os"
	"sync"

	"github.com/hashicorp/yamux"
)

// Registry is safe for concurrent use.
type Registry struct {
	mu       sync.RWMutex
	sessions map[string]*yamux.Session // id -> live tunnel (in-memory)
	secrets  map[string]string         // id -> sha256(secret) hex (persisted)
	path     string                    // persistence file for secrets ("" = in-memory only)
}

// New loads any persisted id->secret bindings from path (created lazily).
func New(path string) (*Registry, error) {
	r := &Registry{
		sessions: make(map[string]*yamux.Session),
		secrets:  make(map[string]string),
		path:     path,
	}
	if err := r.load(); err != nil {
		return nil, err
	}
	return r, nil
}

func (r *Registry) load() error {
	data, err := os.ReadFile(r.path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	if len(data) == 0 {
		return nil
	}
	return json.Unmarshal(data, &r.secrets)
}

func (r *Registry) persistLocked() error {
	if r.path == "" {
		return nil
	}
	data, err := json.MarshalIndent(r.secrets, "", "  ")
	if err != nil {
		return err
	}
	tmp := r.path + ".tmp"
	if err := os.WriteFile(tmp, data, 0o600); err != nil {
		return err
	}
	return os.Rename(tmp, r.path)
}

func hashSecret(secret string) string {
	sum := sha256.Sum256([]byte(secret))
	return hex.EncodeToString(sum[:])
}

// Authenticate binds id->secret on first use and rejects a mismatched secret
// on subsequent use, preventing subdomain hijacking.
func (r *Registry) Authenticate(id, secret string) error {
	if id == "" || secret == "" {
		return errors.New("id and secret are required")
	}
	hash := hashSecret(secret)
	r.mu.Lock()
	defer r.mu.Unlock()
	existing, ok := r.secrets[id]
	if !ok {
		r.secrets[id] = hash
		return r.persistLocked()
	}
	if existing != hash {
		return errors.New("secret does not match the registered identity")
	}
	return nil
}

// SetSession registers the live session for id, closing any previous one.
func (r *Registry) SetSession(id string, sess *yamux.Session) {
	r.mu.Lock()
	old := r.sessions[id]
	r.sessions[id] = sess
	r.mu.Unlock()
	if old != nil && old != sess {
		_ = old.Close()
	}
}

// ClearSession removes the session for id only if it is still the current one.
func (r *Registry) ClearSession(id string, sess *yamux.Session) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.sessions[id] == sess {
		delete(r.sessions, id)
	}
}

// Session returns the live session for id, if any.
func (r *Registry) Session(id string) (*yamux.Session, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	sess, ok := r.sessions[id]
	return sess, ok
}
