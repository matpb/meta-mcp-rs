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
	if HistoryBatchMax != 200 || HistoryBatchBytes != 4<<20 || AttachmentID != "0" {
		t.Errorf("constants changed: %d %d %q", HistoryBatchMax, HistoryBatchBytes, AttachmentID)
	}
}

func TestBatchesSized(t *testing.T) {
	sizes := func(items []int, maxCount, maxBytes int) []int {
		var out []int
		for _, b := range BatchesSized(items, maxCount, maxBytes, func(v int) int { return v }) {
			out = append(out, len(b))
		}
		return out
	}
	tests := []struct {
		name     string
		items    []int
		maxCount int
		maxBytes int
		want     []int
	}{
		{"empty", nil, 10, 100, nil},
		{"count wins", []int{1, 1, 1, 1, 1}, 2, 100, []int{2, 2, 1}},
		{"bytes win", []int{40, 40, 40, 40}, 10, 100, []int{2, 2}},
		{"exact fit", []int{50, 50, 50}, 10, 100, []int{2, 1}},
		{"oversize item alone", []int{10, 500, 10, 10}, 10, 100, []int{1, 1, 2}},
		{"oversize first", []int{500, 10}, 10, 100, []int{1, 1}},
		{"defaults", []int{1, 1, 1}, 0, 0, []int{3}},
	}
	for _, tc := range tests {
		if got := sizes(tc.items, tc.maxCount, tc.maxBytes); len(got) != len(tc.want) || (len(got) > 0 && !equalInts(got, tc.want)) {
			t.Errorf("%s: sizes %v, want %v", tc.name, got, tc.want)
		}
	}
	items := []int{5, 6, 7, 8, 9}
	got := BatchesSized(items, 2, 100, func(int) int { return 1 })
	got[0] = append(got[0], 99)
	if items[2] != 7 {
		t.Errorf("append to a sized batch overwrote the next one: %v", items)
	}
}

func equalInts(a, b []int) bool {
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return len(a) == len(b)
}
