/* Freestanding executable used by the offline signing regression tests.
 * Regenerate with LLVM:
 * clang --target=arm64-apple-ios12.0 -ffreestanding -c minimal.c -o minimal.o
 * ld64.lld -arch arm64 -platform_version ios 12.0 12.0 -e _main minimal.o -o minimal-arm64.macho
 */
int main(void) { return 0; }
