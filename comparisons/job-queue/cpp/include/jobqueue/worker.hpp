#pragma once

#include <string>
#include <vector>

namespace jobqueue {

// Internal representation of a registered worker.
struct Worker {
    std::string id;
    std::vector<std::string> capabilities;
};

// Read-only view returned by JobQueue::list_workers().
struct WorkerView {
    std::string id;
    std::vector<std::string> capabilities;
};

} // namespace jobqueue
