// Package proxy serves inbound friend requests on the share host, routing each
// to the right tunnel client by the FIRST PATH SEGMENT of the request:
//
//	https://share.neko.arm.moe/<id>/v1/...  ->  client <id>'s local /v1/...
package proxy

import (
	"bufio"
	"io"
	"net/http"
	"strings"

	"nekoshare/internal/protocol"
	"nekoshare/internal/registry"
)

// Handler routes friend requests to tunnels by path-prefix identity.
type Handler struct {
	Registry *registry.Registry
}

// splitIdentity splits "/<id>/rest..." into ("<id>", "/rest..."). When there is
// no remainder it returns ("<id>", "/").
func splitIdentity(path string) (string, string) {
	trimmed := strings.TrimPrefix(path, "/")
	slash := strings.IndexByte(trimmed, '/')
	if slash < 0 {
		return trimmed, "/"
	}
	return trimmed[:slash], trimmed[slash:]
}

func (h *Handler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	id, rest := splitIdentity(r.URL.Path)
	if !protocol.ValidID(id) {
		http.Error(w, "unknown share path", http.StatusNotFound)
		return
	}
	sess, ok := h.Registry.Session(id)
	if !ok {
		http.Error(w, "share target is offline", http.StatusBadGateway)
		return
	}
	stream, err := sess.OpenStream()
	if err != nil {
		h.Registry.ClearSession(id, sess)
		http.Error(w, "share tunnel unavailable", http.StatusBadGateway)
		return
	}
	defer stream.Close()

	// Strip the /<id> prefix so the local instance sees its own /v1/... path,
	// mark the request as tunneled (forces token auth past the loopback trust),
	// and drop the hop-by-hop Connection header. The friend's Authorization
	// (the share token) is preserved.
	r.URL.Path = rest
	r.RequestURI = ""
	r.Header.Set(protocol.ShareHeader, "1")
	r.Header.Del("Connection")

	if err := r.Write(stream); err != nil {
		http.Error(w, "failed to forward request", http.StatusBadGateway)
		return
	}
	// The client relays the local response framed by Content-Length or chunked
	// encoding, so ReadResponse completes without needing the stream to close.
	resp, err := http.ReadResponse(bufio.NewReader(stream), r)
	if err != nil {
		http.Error(w, "failed to read tunnel response", http.StatusBadGateway)
		return
	}
	defer resp.Body.Close()

	for key, values := range resp.Header {
		for _, v := range values {
			w.Header().Add(key, v)
		}
	}
	w.WriteHeader(resp.StatusCode)
	flushCopy(w, resp.Body)
}

// flushCopy streams the body to the friend, flushing after each read so SSE
// events arrive immediately rather than being buffered.
func flushCopy(w http.ResponseWriter, body io.Reader) {
	flusher, _ := w.(http.Flusher)
	buf := make([]byte, 32*1024)
	for {
		n, rerr := body.Read(buf)
		if n > 0 {
			if _, werr := w.Write(buf[:n]); werr != nil {
				return
			}
			if flusher != nil {
				flusher.Flush()
			}
		}
		if rerr != nil {
			return
		}
	}
}
