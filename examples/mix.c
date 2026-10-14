#include <stdint.h>

uint32_t mix(uint32_t x, uint32_t y) {
    uint32_t z = x * 33u + y;
    if (z > 100u) {
        return z ^ x;
    }
    return z + 7u;
}
