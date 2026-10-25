//go:build unix

package server

import "os"

// dirModeLoose reports a data_dir whose mode is not 0700.
func dirModeLoose(fi os.FileInfo) bool { return fi.Mode().Perm() != 0o700 }
