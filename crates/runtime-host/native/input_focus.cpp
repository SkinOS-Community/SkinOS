// ARM64 CGameID return ABI adapter. This is not a Steam API implementation:
// every read calls the validated genuine Valve getter supplied by the host.
#include <cstdint>
#include <type_traits>

namespace {
// The observed getter writes its eight-byte return object via x8. A non-trivial
// C++ return object makes Clang generate this ABI without handwritten assembly.
struct GameIDReturn {
    std::uint64_t value;
    ~GameIDReturn() {}
};
static_assert(sizeof(GameIDReturn) == 8);
static_assert(!std::is_trivially_destructible<GameIDReturn>::value);
using Getter = GameIDReturn (*)(void *);
}

extern "C" bool skinos_input_read_focus_gameid(
    void *utils, const void *getter, std::uint64_t *output) {
    if (!utils || !getter || !output) return false;
    const auto result = reinterpret_cast<Getter>(const_cast<void *>(getter))(utils);
    *output = result.value;
    return true;
}

#ifdef SKINOS_INPUT_FOCUS_TEST
#include <cassert>
__attribute__((noinline)) GameIDReturn fixture_getter(void *state) {
    return {*static_cast<std::uint64_t *>(state)};
}
int main() {
    std::uint64_t state = 648800, result = 0;
    assert(skinos_input_read_focus_gameid(&state,
        reinterpret_cast<const void *>(&fixture_getter), &result));
    assert(result == state);
    state = 0;
    assert(skinos_input_read_focus_gameid(&state,
        reinterpret_cast<const void *>(&fixture_getter), &result));
    assert(result == 0);
    assert(!skinos_input_read_focus_gameid(nullptr, nullptr, &result));
}
#endif
