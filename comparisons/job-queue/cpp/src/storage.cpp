#include "jobqueue/storage.hpp"

#include <chrono>
#include <cstdint>
#include <cstring>
#include <fstream>
#include <limits>
#include <stdexcept>
#include <string>
#include <system_error>
#include <vector>

namespace jobqueue {

// ---- self-contained little-endian binary format ------------------------
//
// We persist the queue by snapshotting the entire state on every mutation
// and atomically replacing the previous file. Format is little-endian and
// length-prefixed; payload bytes are stored opaquely.
//
//   u32  magic = 0x4A515633 ('JQV3')
//   u64  next_id
//   u64  next_seq
//   u64  next_event_id
//   u64  oldest_retained_event_id
//
//   u32  job_count
//     per job:
//       u64  id
//       string namespace_id
//       string payload
//       u32  attempt
//       u32  priority
//       u64  enqueue_seq
//       u8   state           (0=Pending..5=Cancelled)
//       u16  flags           (bit 0: lease_holder, 1: lease_expires_at,
//                             bit 2: retry_ready_at, 3: final_reason,
//                             bit 4: scheduled_at, 5: depends_on,
//                             bit 6: required_capabilities)
//       if flags & 0x01: string  lease_holder
//       if flags & 0x02: i64     lease_expires_at_nanos
//       if flags & 0x04: i64     retry_ready_at_nanos
//       if flags & 0x08: string  final_reason
//       if flags & 0x10: i64     scheduled_at_nanos
//       if flags & 0x20: u32 n + [u64; n]    depends_on
//       if flags & 0x40: u32 n + [string; n] required_capabilities
//
//   u32  namespace_count
//     per namespace:
//       string name
//       u64    active_capacity
//       u64    dead_letter_capacity
//       u32 dl_count
//         per dl entry: u64 id; string payload; string final_reason
//       9 × u64 counters in fixed order: enqueued, acquired, completed,
//         failed, lease_expired, dead_lettered, retry_scheduled,
//         cancelled, promoted.
//
//   u32  worker_count
//     per worker: string id; u32 cap_count; [string; n]
//
//   u32  cancelled_count
//     per entry: u64 id; string namespace_id; string payload; string reason
//
//   u32  audit_count
//     per event:
//       u64 event_id; i64 at_nanos; u8 kind; u8 flags
//       (bit 0: has job_id, bit 1: has worker_id);
//       if has job_id: u64; if has worker_id: string; string payload
//
// We picked a custom binary format over JSON to keep the project
// dependency-free. The magic bumped from JQV2 to JQV3 when the v2 fields
// were added, so older files load as bad-magic rather than silent corruption.

namespace {

constexpr std::uint32_t kMagic = 0x4A515633u; // 'JQV3'

// ----------- primitive encoders / decoders -----------

void put_u8(std::vector<std::uint8_t>& out, std::uint8_t v) {
    out.push_back(v);
}
void put_u16(std::vector<std::uint8_t>& out, std::uint16_t v) {
    out.push_back(static_cast<std::uint8_t>(v));
    out.push_back(static_cast<std::uint8_t>(v >> 8));
}
void put_u32(std::vector<std::uint8_t>& out, std::uint32_t v) {
    for (int i = 0; i < 4; ++i) out.push_back(static_cast<std::uint8_t>(v >> (i * 8)));
}
void put_u64(std::vector<std::uint8_t>& out, std::uint64_t v) {
    for (int i = 0; i < 8; ++i) out.push_back(static_cast<std::uint8_t>(v >> (i * 8)));
}
void put_i64(std::vector<std::uint8_t>& out, std::int64_t v) {
    std::uint64_t u;
    std::memcpy(&u, &v, sizeof(u));
    put_u64(out, u);
}
void put_str(std::vector<std::uint8_t>& out, const std::string& s) {
    if (s.size() > std::numeric_limits<std::uint32_t>::max()) {
        throw std::runtime_error("jobqueue: string too long to persist");
    }
    put_u32(out, static_cast<std::uint32_t>(s.size()));
    out.insert(out.end(), s.begin(), s.end());
}

struct Reader {
    const std::uint8_t* p;
    const std::uint8_t* end;

    void need(std::size_t n) {
        if (static_cast<std::size_t>(end - p) < n) {
            throw std::runtime_error("jobqueue: truncated persistence file");
        }
    }
    std::uint8_t  u8()  { need(1); return *p++; }
    std::uint16_t u16() {
        need(2);
        std::uint16_t v = static_cast<std::uint16_t>(p[0])
                        | static_cast<std::uint16_t>(p[1] << 8);
        p += 2;
        return v;
    }
    std::uint32_t u32() {
        need(4);
        std::uint32_t v = 0;
        for (int i = 0; i < 4; ++i) v |= static_cast<std::uint32_t>(p[i]) << (i * 8);
        p += 4;
        return v;
    }
    std::uint64_t u64() {
        need(8);
        std::uint64_t v = 0;
        for (int i = 0; i < 8; ++i) v |= static_cast<std::uint64_t>(p[i]) << (i * 8);
        p += 8;
        return v;
    }
    std::int64_t i64() {
        std::uint64_t u = u64();
        std::int64_t v;
        std::memcpy(&v, &u, sizeof(v));
        return v;
    }
    std::string str() {
        std::uint32_t n = u32();
        need(n);
        std::string s(reinterpret_cast<const char*>(p), n);
        p += n;
        return s;
    }
};

std::int64_t to_nanos(TimePoint tp) {
    return std::chrono::duration_cast<std::chrono::nanoseconds>(
               tp.time_since_epoch())
        .count();
}

TimePoint from_nanos(std::int64_t n) {
    return TimePoint{} + std::chrono::nanoseconds{n};
}

// ----------- per-section helpers -----------

void encode_job(std::vector<std::uint8_t>& out, const Job& j) {
    put_u64(out, j.id);
    put_str(out, j.namespace_id);
    put_str(out, j.payload);
    put_u32(out, j.attempt);
    put_u32(out, j.priority);
    put_u64(out, j.enqueue_seq);
    put_u8(out, static_cast<std::uint8_t>(j.state));
    std::uint16_t flags = 0;
    if (j.lease_holder)                  flags |= 0x01u;
    if (j.lease_expires_at)              flags |= 0x02u;
    if (j.retry_ready_at)                flags |= 0x04u;
    if (j.final_reason)                  flags |= 0x08u;
    if (j.scheduled_at)                  flags |= 0x10u;
    if (!j.depends_on.empty())           flags |= 0x20u;
    if (!j.required_capabilities.empty()) flags |= 0x40u;
    put_u16(out, flags);
    if (j.lease_holder)     put_str(out, *j.lease_holder);
    if (j.lease_expires_at) put_i64(out, to_nanos(*j.lease_expires_at));
    if (j.retry_ready_at)   put_i64(out, to_nanos(*j.retry_ready_at));
    if (j.final_reason)     put_str(out, *j.final_reason);
    if (j.scheduled_at)     put_i64(out, to_nanos(*j.scheduled_at));
    if (!j.depends_on.empty()) {
        put_u32(out, static_cast<std::uint32_t>(j.depends_on.size()));
        for (auto id : j.depends_on) put_u64(out, id);
    }
    if (!j.required_capabilities.empty()) {
        put_u32(out, static_cast<std::uint32_t>(j.required_capabilities.size()));
        for (const auto& s : j.required_capabilities) put_str(out, s);
    }
}

Job decode_job(Reader& r) {
    Job j;
    j.id           = r.u64();
    j.namespace_id = r.str();
    j.payload      = r.str();
    j.attempt      = r.u32();
    j.priority     = r.u32();
    j.enqueue_seq  = r.u64();
    j.state        = static_cast<JobState>(r.u8());
    std::uint16_t flags = r.u16();
    if (flags & 0x01u) j.lease_holder      = r.str();
    if (flags & 0x02u) j.lease_expires_at  = from_nanos(r.i64());
    if (flags & 0x04u) j.retry_ready_at    = from_nanos(r.i64());
    if (flags & 0x08u) j.final_reason      = r.str();
    if (flags & 0x10u) j.scheduled_at      = from_nanos(r.i64());
    if (flags & 0x20u) {
        std::uint32_t n = r.u32();
        j.depends_on.reserve(n);
        for (std::uint32_t i = 0; i < n; ++i) j.depends_on.push_back(r.u64());
    }
    if (flags & 0x40u) {
        std::uint32_t n = r.u32();
        j.required_capabilities.reserve(n);
        for (std::uint32_t i = 0; i < n; ++i) j.required_capabilities.push_back(r.str());
    }
    return j;
}

void encode_counters(std::vector<std::uint8_t>& out, const Metrics& m) {
    put_u64(out, m.enqueued_total);
    put_u64(out, m.acquired_total);
    put_u64(out, m.completed_total);
    put_u64(out, m.failed_total);
    put_u64(out, m.lease_expired_total);
    put_u64(out, m.dead_lettered_total);
    put_u64(out, m.retry_scheduled_total);
    put_u64(out, m.cancelled_total);
    put_u64(out, m.promoted_total);
}

Metrics decode_counters(Reader& r) {
    Metrics m;
    m.enqueued_total        = r.u64();
    m.acquired_total        = r.u64();
    m.completed_total       = r.u64();
    m.failed_total          = r.u64();
    m.lease_expired_total   = r.u64();
    m.dead_lettered_total   = r.u64();
    m.retry_scheduled_total = r.u64();
    m.cancelled_total       = r.u64();
    m.promoted_total        = r.u64();
    return m;
}

void encode_audit(std::vector<std::uint8_t>& out, const AuditEvent& e) {
    put_u64(out, e.event_id);
    put_i64(out, e.at_nanos);
    put_u8(out, static_cast<std::uint8_t>(e.kind));
    std::uint8_t flags = 0;
    if (e.job_id)    flags |= 0x01u;
    if (e.worker_id) flags |= 0x02u;
    put_u8(out, flags);
    if (e.job_id)    put_u64(out, *e.job_id);
    if (e.worker_id) put_str(out, *e.worker_id);
    put_str(out, e.payload);
}

AuditEvent decode_audit(Reader& r) {
    AuditEvent e;
    e.event_id = r.u64();
    e.at_nanos = r.i64();
    e.kind     = static_cast<AuditEventKind>(r.u8());
    std::uint8_t flags = r.u8();
    if (flags & 0x01u) e.job_id    = r.u64();
    if (flags & 0x02u) e.worker_id = r.str();
    e.payload = r.str();
    return e;
}

// ----------- top-level encode / decode -----------

std::vector<std::uint8_t> encode(const PersistedState& s) {
    std::vector<std::uint8_t> out;
    out.reserve(128 + s.jobs.size() * 96 + s.audit.size() * 64);

    put_u32(out, kMagic);
    put_u64(out, s.next_id);
    put_u64(out, s.next_seq);
    put_u64(out, s.next_event_id);
    put_u64(out, s.oldest_retained_event_id);

    put_u32(out, static_cast<std::uint32_t>(s.jobs.size()));
    for (const auto& j : s.jobs) encode_job(out, j);

    put_u32(out, static_cast<std::uint32_t>(s.namespaces.size()));
    for (const auto& [name, ns] : s.namespaces) {
        put_str(out, name);
        put_u64(out, ns.config.active_capacity);
        put_u64(out, ns.config.dead_letter_capacity);
        put_u32(out, static_cast<std::uint32_t>(ns.dead_letter.size()));
        for (const auto& d : ns.dead_letter) {
            put_u64(out, d.id);
            put_str(out, d.payload);
            put_str(out, d.final_reason);
        }
        encode_counters(out, ns.counters);
    }

    put_u32(out, static_cast<std::uint32_t>(s.workers.size()));
    for (const auto& [id, w] : s.workers) {
        put_str(out, w.id);
        put_u32(out, static_cast<std::uint32_t>(w.capabilities.size()));
        for (const auto& c : w.capabilities) put_str(out, c);
    }

    put_u32(out, static_cast<std::uint32_t>(s.cancelled.size()));
    for (const auto& c : s.cancelled) {
        put_u64(out, c.id);
        put_str(out, c.namespace_id);
        put_str(out, c.payload);
        put_str(out, c.reason);
    }

    put_u32(out, static_cast<std::uint32_t>(s.audit.size()));
    for (const auto& e : s.audit) encode_audit(out, e);

    return out;
}

PersistedState decode(const std::vector<std::uint8_t>& bytes) {
    Reader r{bytes.data(), bytes.data() + bytes.size()};
    if (r.u32() != kMagic) {
        throw std::runtime_error("jobqueue: bad magic in persistence file");
    }
    PersistedState s;
    s.next_id                  = r.u64();
    s.next_seq                 = r.u64();
    s.next_event_id            = r.u64();
    s.oldest_retained_event_id = r.u64();

    std::uint32_t n_jobs = r.u32();
    s.jobs.reserve(n_jobs);
    for (std::uint32_t i = 0; i < n_jobs; ++i) s.jobs.push_back(decode_job(r));

    std::uint32_t n_ns = r.u32();
    for (std::uint32_t i = 0; i < n_ns; ++i) {
        std::string name = r.str();
        PersistedNamespace ns;
        ns.config.active_capacity      = static_cast<std::size_t>(r.u64());
        ns.config.dead_letter_capacity = static_cast<std::size_t>(r.u64());
        std::uint32_t n_dl = r.u32();
        ns.dead_letter.reserve(n_dl);
        for (std::uint32_t k = 0; k < n_dl; ++k) {
            DeadLetterEntry d;
            d.id           = r.u64();
            d.payload      = r.str();
            d.final_reason = r.str();
            ns.dead_letter.push_back(std::move(d));
        }
        ns.counters = decode_counters(r);
        s.namespaces.emplace(std::move(name), std::move(ns));
    }

    std::uint32_t n_workers = r.u32();
    for (std::uint32_t i = 0; i < n_workers; ++i) {
        Worker w;
        w.id = r.str();
        std::uint32_t n_caps = r.u32();
        w.capabilities.reserve(n_caps);
        for (std::uint32_t k = 0; k < n_caps; ++k) w.capabilities.push_back(r.str());
        std::string id_copy = w.id;
        s.workers.emplace(std::move(id_copy), std::move(w));
    }

    std::uint32_t n_cancelled = r.u32();
    s.cancelled.reserve(n_cancelled);
    for (std::uint32_t i = 0; i < n_cancelled; ++i) {
        CancelledEntry c;
        c.id           = r.u64();
        c.namespace_id = r.str();
        c.payload      = r.str();
        c.reason       = r.str();
        s.cancelled.push_back(std::move(c));
    }

    std::uint32_t n_audit = r.u32();
    for (std::uint32_t i = 0; i < n_audit; ++i) s.audit.push_back(decode_audit(r));

    return s;
}

} // namespace

// ----------------------------- MemoryStorage -----------------------------

void MemoryStorage::save(const PersistedState& state) {
    std::lock_guard<std::mutex> lock(mu_);
    state_ = state;
}

std::optional<PersistedState> MemoryStorage::load() {
    std::lock_guard<std::mutex> lock(mu_);
    return state_;
}

// ----------------------------- FileStorage -----------------------------

FileStorage::FileStorage(std::filesystem::path path)
    : path_(std::move(path)) {
    tmp_path_ = path_;
    tmp_path_ += ".tmp";
    if (path_.has_parent_path()) {
        std::error_code ec;
        std::filesystem::create_directories(path_.parent_path(), ec);
    }
}

void FileStorage::save(const PersistedState& state) {
    std::lock_guard<std::mutex> lock(mu_);
    const auto bytes = encode(state);
    {
        std::ofstream out(tmp_path_, std::ios::binary | std::ios::trunc);
        if (!out) throw std::runtime_error("FileStorage: cannot open temp file");
        out.write(reinterpret_cast<const char*>(bytes.data()),
                  static_cast<std::streamsize>(bytes.size()));
        out.flush();
        if (!out) throw std::runtime_error("FileStorage: write failed");
    }
    std::error_code ec;
    std::filesystem::rename(tmp_path_, path_, ec);
    if (ec) {
        // On some platforms rename refuses to overwrite. Fall back to
        // remove-then-rename. (Crash window between the two leaves the
        // temp file behind, recoverable manually.)
        std::filesystem::remove(path_, ec);
        std::filesystem::rename(tmp_path_, path_, ec);
        if (ec) throw std::system_error(ec, "FileStorage: atomic rename failed");
    }
}

std::optional<PersistedState> FileStorage::load() {
    std::lock_guard<std::mutex> lock(mu_);
    std::error_code ec;
    if (!std::filesystem::exists(path_, ec)) return std::nullopt;

    std::ifstream in(path_, std::ios::binary);
    if (!in) return std::nullopt;
    std::vector<std::uint8_t> bytes(
        (std::istreambuf_iterator<char>(in)),
        std::istreambuf_iterator<char>());
    if (bytes.empty()) return std::nullopt;
    return decode(bytes);
}

} // namespace jobqueue
