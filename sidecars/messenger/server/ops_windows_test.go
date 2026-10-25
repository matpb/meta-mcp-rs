//go:build windows

package server

import "testing"

func TestIsAbsCleanWindowsPaths(t *testing.T) {
	for p, want := range map[string]bool{
		`C:\Users\x\AppData\Roaming\meta-mcp\config\personal\whatsapp`: true,
		`C:\Users\x\..\y`: false,
		`C:\Users\x\`:     false,
		`C:/Users/x`:      false,
		`relative\dir`:    false,
	} {
		if got := isAbsClean(p); got != want {
			t.Errorf("isAbsClean(%q) = %v, want %v", p, got, want)
		}
	}
}
