// Package control authenticates a tunnel client over an HTTP/1.1 upgrade,
// hijacks the connection, and runs one yamux session per client identity.
package control

import (
	"log"
	"net/http"
	"strings"

	"github.com/hashicorp/yamux"

	"nekoshare/internal/protocol"
	"nekoshare/internal/registry"
)

// Handler serves the control upgrade on the server host.
type Handler struct {
	Registry *registry.Registry
}

func (h *Handler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	id := strings.ToLower(strings.TrimSpace(r.Header.Get(protocol.HeaderID)))
	secret := r.Header.Get(protocol.HeaderSecret)
	if !protocol.ValidID(id) {
		http.Error(w, "invalid identity", http.StatusBadRequest)
		return
	}
	if err := h.Registry.Authenticate(id, secret); err != nil {
		http.Error(w, err.Error(), http.StatusUnauthorized)
		return
	}
	hijacker, ok := w.(http.Hijacker)
	if !ok {
		http.Error(w, "connection cannot be hijacked", http.StatusInternalServerError)
		return
	}
	conn, buf, err := hijacker.Hijack()
	if err != nil {
		return
	}
	defer conn.Close()
	if buf.Reader.Buffered() > 0 {
		// Client pipelined bytes before the 101 — protocol violation; drop it
		// rather than desync yamux.
		log.Printf("[control] id=%s sent data before upgrade ack; dropping", id)
		return
	}
	ack := "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: " +
		protocol.UpgradeToken + "\r\n\r\n"
	if _, err := buf.WriteString(ack); err != nil {
		return
	}
	if err := buf.Flush(); err != nil {
		return
	}

	// Server OPENS one stream per inbound request, so it is the yamux client.
	cfg := yamux.DefaultConfig()
	cfg.MaxStreamWindowSize = 1024 * 1024 // 256KB→1MiB：提升响应/大下行(如图片)吞吐
	sess, err := yamux.Client(conn, cfg)
	if err != nil {
		log.Printf("[control] yamux for %s: %v", id, err)
		return
	}
	h.Registry.SetSession(id, sess)
	log.Printf("[control] tunnel up id=%s", id)
	<-sess.CloseChan()
	h.Registry.ClearSession(id, sess)
	log.Printf("[control] tunnel down id=%s", id)
}
