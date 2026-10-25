package wa

import "runtime"

// posixModes is false on Windows, where file modes are synthetic and 0600 cannot be asserted.
const posixModes = runtime.GOOS != "windows"
