package protocol

import "testing"

func TestValidID(t *testing.T) {
	cases := []struct {
		id   string
		want bool
	}{
		{"abcd1234efgh5678", true},
		{"0000000000000000", true},
		{"short", false},
		{"abcd1234efgh567", false},  // 15 chars
		{"abcd1234efgh56789", false}, // 17 chars
		{"ABCD1234EFGH5678", false},  // uppercase
		{"abcd-234efgh5678", false},  // symbol
	}
	for _, c := range cases {
		if got := ValidID(c.id); got != c.want {
			t.Errorf("ValidID(%q) = %v, want %v", c.id, got, c.want)
		}
	}
}
