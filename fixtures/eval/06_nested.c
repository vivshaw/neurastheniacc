uint32_t nested(uint32_t x, uint32_t y) {
    if (x > 100u) {
        if (y <= 7u) { return x - y; }
        return x + y;
    }
    return x ^ y;
}
