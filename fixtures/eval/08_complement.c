uint32_t complement(uint32_t x, uint32_t y) {
    return (~x & y) | (x & ~y);
}
