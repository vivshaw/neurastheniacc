uint32_t shadow(uint32_t x, uint32_t y) {
    uint32_t z = x + 1u;
    if (y != 0u) {
        uint32_t z = y + 2u;
        x = z;
    }
    return z ^ x;
}
