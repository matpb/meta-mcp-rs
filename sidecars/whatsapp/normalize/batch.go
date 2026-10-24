package normalize

// HistoryBatchMax is the most messages one history event carries.
const HistoryBatchMax = 200

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
