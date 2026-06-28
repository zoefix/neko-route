// Command nekoshare is the Neko Route share tunnel server.
//
// One Go binary terminates TLS for two hosts on :443 (Let's Encrypt via
// autocert) and routes by Host:
//
//	server.neko.arm.moe  -> control: HTTP/1.1 upgrade -> hijack -> yamux session
//	share.neko.arm.moe   -> proxy:   https://share.../<id>/v1/... -> tunnel <id>
//
// :80 serves ACME HTTP-01 challenges and redirects everything else to HTTPS.
package main

import (
	"flag"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"strings"

	"golang.org/x/crypto/acme/autocert"

	"nekoshare/internal/control"
	"nekoshare/internal/proxy"
	"nekoshare/internal/registry"
)

func main() {
	var (
		shareHost  = flag.String("share-host", "share.neko.arm.moe", "host friends connect to (path-routed)")
		serverHost = flag.String("server-host", "server.neko.arm.moe", "host tunnel clients connect to")
		httpAddr   = flag.String("http", ":80", "HTTP listener (ACME HTTP-01 + redirect)")
		httpsAddr  = flag.String("https", ":443", "HTTPS listener")
		statePath  = flag.String("state", "/var/lib/nekoshare/state.json", "persistent id->secret store")
		certDir    = flag.String("cert-cache", "/var/lib/nekoshare/certs", "autocert certificate cache dir")
		email      = flag.String("email", "", "ACME contact email (optional)")
	)
	flag.Parse()

	for _, dir := range []string{filepath.Dir(*statePath), *certDir} {
		if dir != "" && dir != "." {
			if err := os.MkdirAll(dir, 0o700); err != nil {
				log.Fatalf("mkdir %s: %v", dir, err)
			}
		}
	}
	reg, err := registry.New(*statePath)
	if err != nil {
		log.Fatalf("registry: %v", err)
	}

	manager := &autocert.Manager{
		Prompt:     autocert.AcceptTOS,
		HostPolicy: autocert.HostWhitelist(*shareHost, *serverHost),
		Cache:      autocert.DirCache(*certDir),
		Email:      *email,
	}

	controlHandler := &control.Handler{Registry: reg}
	proxyHandler := &proxy.Handler{Registry: reg}
	share := strings.ToLower(*shareHost)
	server := strings.ToLower(*serverHost)
	dispatch := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch hostOnly(strings.ToLower(r.Host)) {
		case server:
			controlHandler.ServeHTTP(w, r)
		case share:
			proxyHandler.ServeHTTP(w, r)
		default:
			http.Error(w, "unknown host", http.StatusNotFound)
		}
	})

	// :80 — ACME HTTP-01 challenges; everything else redirected to HTTPS.
	go func() {
		log.Printf("[http] listening on %s", *httpAddr)
		if err := http.ListenAndServe(*httpAddr, manager.HTTPHandler(nil)); err != nil {
			log.Fatalf("http listener: %v", err)
		}
	}()

	srv := &http.Server{
		Addr:      *httpsAddr,
		Handler:   dispatch,
		TLSConfig: manager.TLSConfig(),
	}
	log.Printf("[https] listening on %s  (share=%s  server=%s)", *httpsAddr, *shareHost, *serverHost)
	log.Fatalf("https listener: %v", srv.ListenAndServeTLS("", ""))
}

func hostOnly(host string) string {
	if i := strings.IndexByte(host, ':'); i >= 0 {
		return host[:i]
	}
	return host
}
