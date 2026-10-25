//go:build windows

package server

import "os"

// dirModeLoose is always false: Windows reports synthetic modes and access is governed by ACLs.
func dirModeLoose(os.FileInfo) bool { return false }
