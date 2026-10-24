package normalize

// HistoryBatchMax is the most messages one history event carries.
const HistoryBatchMax = 200

// HistoryBatchBytes is the serialised size one history event stays under, a quarter of the 16 MiB frame limit.
const HistoryBatchBytes = 4 << 20

// AttachmentID is the id of the single attachment of a WhatsApp message.
const AttachmentID = "0"

// Batches splits items into consecutive chunks of at most max (HistoryBatchMax when max < 1).
// The chunks alias items; empty input gives no batches.
func Batches[T any](items []T, max int) [][]T {
	if max < 1 {
		max = HistoryBatchMax
	}
	out := make([][]T, 0, (len(items)+max-1)/max)
	for i := 0; i < len(items); i += max {
		j := min(i+max, len(items))
		out = append(out, items[i:j:j])
	}
	return out
}

// BatchesSized is Batches that also closes a chunk before its summed size(item) would pass maxBytes.
// An item over maxBytes gets a chunk of its own; maxBytes < 1 means HistoryBatchBytes.
func BatchesSized[T any](items []T, maxCount, maxBytes int, size func(T) int) [][]T {
	if maxCount < 1 {
		maxCount = HistoryBatchMax
	}
	if maxBytes < 1 {
		maxBytes = HistoryBatchBytes
	}
	var out [][]T
	start, used := 0, 0
	for i, it := range items {
		n := size(it)
		if i > start && (i-start >= maxCount || used+n > maxBytes) {
			out = append(out, items[start:i:i])
			start, used = i, 0
		}
		used += n
	}
	if start < len(items) {
		out = append(out, items[start:len(items):len(items)])
	}
	return out
}
