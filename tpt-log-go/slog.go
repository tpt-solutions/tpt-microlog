package microlog

import (
	"context"
	"fmt"
	"log/slog"
	"time"
)

// LevelFatal is the slog level mapped to tpt-microlog's FATAL severity. slog
// has no such level; use it with log.Log(ctx, microlog.LevelFatal, "...").
const LevelFatal = slog.LevelError + 4

// SlogHandler is a log/slog.Handler that forwards records to a MicroLog.
//
//	logger := slog.New(microlog.NewSlogHandler(log, nil))
//	logger.Info("user logged in", "user_id", 123)
//
// Mapping: levels below DEBUG become TRACE, then DEBUG/INFO/WARN/ERROR, and
// LevelFatal and above become FATAL. Attributes become metadata; groups
// become nested objects. The handler never closes the MicroLog.
type SlogHandler struct {
	log    *MicroLog
	level  slog.Leveler
	groups []string       // open groups, outermost first
	attrs  map[string]any // pre-resolved WithAttrs data, already nested
}

// NewSlogHandler builds a handler over log. opts may be nil; only Level is
// honored (default INFO, like slog's built-in handlers).
func NewSlogHandler(log *MicroLog, opts *slog.HandlerOptions) *SlogHandler {
	var level slog.Leveler = slog.LevelInfo
	if opts != nil && opts.Level != nil {
		level = opts.Level
	}
	return &SlogHandler{log: log, level: level}
}

// Enabled reports whether records at level are handled.
func (h *SlogHandler) Enabled(_ context.Context, level slog.Level) bool {
	return level >= h.level.Level()
}

// Handle forwards one record.
func (h *SlogHandler) Handle(_ context.Context, r slog.Record) error {
	md := cloneMap(h.attrs)
	target := descend(md, h.groups)
	r.Attrs(func(a slog.Attr) bool {
		addAttr(target, a)
		return true
	})
	if len(md) == 0 {
		md = nil
	}
	switch {
	case r.Level >= LevelFatal:
		return h.log.Fatal(r.Message, md)
	case r.Level >= slog.LevelError:
		return h.log.Error(r.Message, md)
	case r.Level >= slog.LevelWarn:
		return h.log.Warn(r.Message, md)
	case r.Level >= slog.LevelInfo:
		return h.log.Info(r.Message, md)
	case r.Level >= slog.LevelDebug:
		return h.log.Debug(r.Message, md)
	default:
		return h.log.Trace(r.Message, md)
	}
}

// WithAttrs returns a handler whose records always carry attrs.
func (h *SlogHandler) WithAttrs(attrs []slog.Attr) slog.Handler {
	if len(attrs) == 0 {
		return h
	}
	next := h.clone()
	target := descend(next.attrs, next.groups)
	for _, a := range attrs {
		addAttr(target, a)
	}
	return next
}

// WithGroup returns a handler that nests subsequent attributes under name.
func (h *SlogHandler) WithGroup(name string) slog.Handler {
	if name == "" {
		return h
	}
	next := h.clone()
	next.groups = append(next.groups, name)
	return next
}

func (h *SlogHandler) clone() *SlogHandler {
	return &SlogHandler{
		log:    h.log,
		level:  h.level,
		groups: append([]string(nil), h.groups...),
		attrs:  cloneMap(h.attrs),
	}
}

// descend returns the nested map at path inside m, creating it as needed.
func descend(m map[string]any, path []string) map[string]any {
	cur := m
	for _, g := range path {
		next, ok := cur[g].(map[string]any)
		if !ok {
			next = map[string]any{}
			cur[g] = next
		}
		cur = next
	}
	return cur
}

// cloneMap deep-copies nested maps so handlers derived from one another never
// share mutable state.
func cloneMap(m map[string]any) map[string]any {
	out := make(map[string]any, len(m))
	for k, v := range m {
		if nested, ok := v.(map[string]any); ok {
			out[k] = cloneMap(nested)
		} else {
			out[k] = v
		}
	}
	return out
}

func addAttr(dst map[string]any, a slog.Attr) {
	a.Value = a.Value.Resolve()
	if a.Equal(slog.Attr{}) {
		return // slog contract: ignore empty attrs
	}
	if a.Value.Kind() == slog.KindGroup {
		group := a.Value.Group()
		if len(group) == 0 {
			return
		}
		if a.Key == "" { // inline group
			for _, ga := range group {
				addAttr(dst, ga)
			}
			return
		}
		nested := map[string]any{}
		for _, ga := range group {
			addAttr(nested, ga)
		}
		dst[a.Key] = nested
		return
	}
	dst[a.Key] = attrValue(a.Value)
}

// attrValue converts a resolved slog.Value into something JSON-friendly.
func attrValue(v slog.Value) any {
	switch v.Kind() {
	case slog.KindTime:
		return v.Time().Format(time.RFC3339Nano)
	case slog.KindDuration:
		return v.Duration().String()
	case slog.KindAny:
		switch x := v.Any().(type) {
		case error:
			return x.Error() // errors marshal to {} otherwise
		case fmt.Stringer:
			return x.String()
		default:
			return x
		}
	default:
		return v.Any()
	}
}
