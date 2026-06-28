// Package protocol defines the small control handshake shared between the
// Neko Route tunnel client (Rust) and this server, plus shared helpers.
package protocol

// ProtocolVersion is the wire-protocol version sent on the control upgrade.
const ProtocolVersion = 1

// ShareHeader marks a request that arrived through the public share tunnel, so
// the local Neko Route instance enforces token auth instead of trusting
// loopback (the tunneled request reaches it on 127.0.0.1).
const ShareHeader = "X-Neko-Share"

// IDLength is the fixed length of a share identity (the path label).
const IDLength = 16

// Control-plane upgrade: the tunnel client opens an HTTPS connection to the
// server host and sends an HTTP/1.1 upgrade request carrying its identity;
// the server authenticates, replies 101, hijacks the connection, and runs
// yamux over it.
const (
	ControlPath   = "/tunnel"
	UpgradeToken  = "neko-tunnel"
	HeaderID      = "X-Neko-Id"
	HeaderSecret  = "X-Neko-Secret"
	HeaderVersion = "X-Neko-Ver"
)

// ValidID reports whether id is a well-formed share identity: exactly
// IDLength lowercase letters or digits.
func ValidID(id string) bool {
	if len(id) != IDLength {
		return false
	}
	for _, c := range id {
		if !((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9')) {
			return false
		}
	}
	return true
}
