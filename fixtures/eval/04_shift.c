uint32_t shift(uint32_t x, uint32_t y) {
    return (x >> 31u) ^ (y << 7u);
}
