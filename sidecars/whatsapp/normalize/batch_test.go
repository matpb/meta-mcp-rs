package normalize

import "testing"

func TestBatches(t *testing.T) {
	seq := func(n int) []int {
		s := make([]int, n)
		for i := range s {
			s[i] = i
		}
		return s
	}
	tests := []struct {
		name  string
		n     int
		max   int
		sizes []int
	}{
		{"450 by 200", 450, HistoryBatchMax, []int{200, 200, 50}},
		{"zero", 0, HistoryBatchMax, []int{}},
		{"exactly 200", 200, HistoryBatchMax, []int{200}},
		{"201", 201, HistoryBatchMax, []int{200, 1}},
		{"one", 1, HistoryBatchMax, []int{1}},
		{"non-positive max uses default", 450, 0, []int{200, 200, 50}},
		{"custom max", 7, 3, []int{3, 3, 1}},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			items := seq(tc.n)
			got := Batches(items, tc.max)
			if len(got) != len(tc.sizes) {
				t.Fatalf("batches = %d, want %d", len(got), len(tc.sizes))
			}
			next := 0
			for i, b := range got {
				if len(b) != tc.sizes[i] {
					t.Errorf("batch %d size = %d, want %d", i, len(b), tc.sizes[i])
				}
				for _, v := range b {
					if v != next {
						t.Fatalf("order broken at %d: got %d", next, v)
					}
					next++
				}
			}
			if next != tc.n {
				t.Errorf("covered %d items, want %d", next, tc.n)
			}
		})
	}
}

func TestBatchesAppendDoesNotClobber(t *testing.T) {
	items := []int{1, 2, 3, 4}
	got := Batches(items, 2)
	got[0] = append(got[0], 99)
	if items[2] != 3 {
		t.Errorf("append to first batch overwrote the next one: %v", items)
	}
}

func TestConstants(t *testing.T) {
	if HistoryBatchMax != 200 || AttachmentID != "0" {
		t.Errorf("constants changed: %d %q", HistoryBatchMax, AttachmentID)
	}
}
